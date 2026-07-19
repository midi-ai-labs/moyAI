use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::io::Read;
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
    AppCommand, ReviewRequest, RunConfigInput, RunRequest, SessionArchiveRequest,
    SessionEventsRequest, SessionForkRequest, SessionGoalClearRequest, SessionGoalGetRequest,
    SessionGoalSetRequest, SessionHistoryRequest, SessionIdleAdmissionRequest,
    SessionInterruptRequest, SessionListRequest, SessionLoadedRequest, SessionReadRequest,
    SessionRejoinRequest, SessionRollbackRequest, SessionSearchRequest,
    SessionSettingsUpdateRequest, SessionShowRequest, SessionSteerRequest,
    SessionTitleUpdateRequest, SessionTurnsRequest,
};
use crate::cli::{ConfirmationPrompt, EventRenderer};
use crate::config::model::PartialResolvedConfig;
use crate::config::{
    ModelConfig, ResolvedConfig, ResolvedTurnConfig, merge::apply_patch as apply_config_patch,
};
use crate::error::{AgentError, AppRunError, RuntimeError};
use crate::harness::{HarnessRecordingSink, NativeHarnessRecorder};
use crate::llm::model_policy::{ModelPolicy, ProviderCapabilities, ResolvedTurnPolicy};
use crate::llm::validate_image_bytes;
use crate::protocol::{
    AdditionalContextEntry, AdditionalContextKind, ProtocolEventStore, ProtocolRecordingSink,
    SteerTurn, UserInputItem, UserTurn,
};
use crate::runtime::{RunCancellationCause, RunControl, RunEventSink, SessionRuntimeEventHub};
use crate::session::{
    AdmissionId, DispatchTransformKind, ImagePart, PromptDispatchPart, RunSummary,
    SessionModelParameters, SessionRecord, SessionRepository, SessionSelector,
    SessionSettingsPatch, SessionStartRequest, SessionStatus, ThreadGoalClearResult,
    ThreadGoalGetResult, ThreadGoalSetResult, ThreadGoalStatus, validate_thread_goal_objective,
};
use crate::storage::{
    StoreBundle,
    session_repo::{
        ActiveGoalTurnAdmission, DirectChildRunAdmissionState, RUN_ADMISSION_HEARTBEAT_INTERVAL_MS,
        RunAdmissionLeaseRenewalOutcome,
    },
};
use crate::workspace::{AccessKind, PathGuard, branch_review_scope, uncommitted_review_scope};

const DEFAULT_SESSION_SHOW_LIMIT: usize = 100;
const MAX_WORKFLOW_COMMAND_BYTES: usize = 16 * 1024;
const MAX_EXPANDED_WORKFLOW_BYTES: usize = 64 * 1024;
const WORKFLOW_ARGUMENT_PLACEHOLDER: &str = "{{args}}";

enum SingleRunOutcome {
    Turn(RunSummary),
    ControlCompleted,
    IdleGoalInactive,
}

#[derive(Debug, Clone)]
pub enum AppCommandOutcome {
    Turn(RunSummary),
    ControlCompleted,
}

fn blocking_direct_child<'a>(
    states: &'a [DirectChildRunAdmissionState],
    active_runs: &crate::runtime::ActiveRunRegistry,
) -> Option<&'a crate::session::SessionSpawnEdge> {
    states
        .iter()
        .find(|state| {
            state.blocks_new_root_turn || active_runs.is_active(state.edge.child_session_id)
        })
        .map(|state| &state.edge)
}

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
    ) -> Result<AppCommandOutcome, AppRunError> {
        match command {
            AppCommand::Run(request) => self.execute_run(request, renderer, prompt).await,
            AppCommand::SessionArchive(request) => self
                .execute_session_archive(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionList(request) => self
                .execute_session_list(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionLoaded(request) => self
                .execute_session_loaded(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionSearch(request) => self
                .execute_session_search(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionSettingsUpdate(request) => self
                .execute_session_settings_update(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionTitleUpdate(request) => self
                .execute_session_title_update(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionInterrupt(request) => self
                .execute_session_interrupt(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionGoalGet(request) => self
                .execute_session_goal_get(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionGoalSet(request) => self
                .execute_session_goal_set(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionGoalClear(request) => self
                .execute_session_goal_clear(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionIdleAdmission(request) => self
                .execute_session_idle_admission(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionShow(request) => self
                .execute_session_show(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionHistory(request) => self
                .execute_session_history(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionRead(request) => self
                .execute_session_read(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionRejoin(request) => self
                .execute_session_rejoin(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionRollback(request) => self
                .execute_session_rollback(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionFork(request) => self
                .execute_session_fork(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionTurns(request) => self
                .execute_session_turns(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionEvents(request) => self
                .execute_session_events(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
            AppCommand::SessionSteer(request) => self
                .execute_session_steer(request, renderer)
                .await
                .map(|()| AppCommandOutcome::ControlCompleted),
        }
    }

    async fn context_manager(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<ContextManager, AppRunError> {
        let mut context_builder = ContextManager::active_history_builder();
        let snapshot = self
            .store
            .protocol_event_store()
            .visit_active_history_pages_for_session(
                session_id,
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                &mut |page| {
                    context_builder.ingest_page(page.items);
                    Ok(())
                },
            )?;
        let context = context_builder.finish(
            snapshot.append_fence,
            snapshot.canonical_count,
            snapshot.steer_count,
            snapshot.agent_communication_count,
        );
        if !context.has_model_context() {
            return Err(AppRunError::Message(
                "cannot build runtime input without active canonical model context".to_string(),
            ));
        }
        Ok(context)
    }

    async fn execute_run(
        &self,
        request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
    ) -> Result<AppCommandOutcome, AppRunError> {
        let allow_idle_goal_continuation = request.agent_context.is_none()
            && allows_goal_idle_continuation_after_run(&request.prompt)?;
        let mut summary = match self
            .execute_single_run(request.clone(), renderer, prompt, None)
            .await?
        {
            SingleRunOutcome::Turn(summary) => summary,
            SingleRunOutcome::ControlCompleted => {
                return Ok(AppCommandOutcome::ControlCompleted);
            }
            SingleRunOutcome::IdleGoalInactive => {
                unreachable!("an explicit run does not require an active goal")
            }
        };
        if !allow_idle_goal_continuation {
            return Ok(AppCommandOutcome::Turn(summary));
        }

        'continuations: loop {
            let preclaimed_root_execution = loop {
                self.wait_for_agent_tree_quiescence(summary.session_id())
                    .await?;
                match self
                    .agent_runtime
                    .begin_root_continuation(
                        summary.session_id(),
                        request.run_control.clone(),
                        request.agent_confirmation.clone(),
                    )
                    .map_err(AppRunError::Message)?
                {
                    AgentRuntimeContinuationOutcome::Admitted(execution) => {
                        break Some(execution);
                    }
                    AgentRuntimeContinuationOutcome::Blocked => break 'continuations,
                    AgentRuntimeContinuationOutcome::NotReady => continue,
                    AgentRuntimeContinuationOutcome::Invalid => {
                        return Err(AppRunError::Message(format!(
                            "session {} could not admit an idle goal continuation from its retained root task scope",
                            summary.session_id()
                        )));
                    }
                }
            };
            let continuation_request = RunRequest {
                prompt: String::new(),
                session_id: Some(summary.session_id()),
                continue_last: false,
                title: None,
                cwd: request.cwd.clone(),
                config: request.config.clone(),
                output_mode: request.output_mode,
                show_reasoning_summary: request.show_reasoning_summary,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                run_control: request.run_control.clone(),
                session_access_mode_adoption: None,
                agent_confirmation: request.agent_confirmation.clone(),
                agent_context: request.agent_context.clone(),
            };
            match self
                .execute_single_run(
                    continuation_request,
                    renderer,
                    prompt,
                    preclaimed_root_execution,
                )
                .await?
            {
                SingleRunOutcome::Turn(next_summary) => summary = next_summary,
                SingleRunOutcome::ControlCompleted => break 'continuations,
                SingleRunOutcome::IdleGoalInactive => break 'continuations,
            }
        }

        Ok(AppCommandOutcome::Turn(summary))
    }

    async fn execute_single_run(
        &self,
        request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
        mut root_agent_execution: Option<AgentRuntimeExecution>,
    ) -> Result<SingleRunOutcome, AppRunError> {
        let root_scope_control = request.run_control.clone();
        let mut turn_run_control = root_agent_execution
            .as_ref()
            .map(AgentRuntimeExecution::run_control);
        let result = self
            .execute_single_run_inner(
                request,
                renderer,
                prompt,
                &mut root_agent_execution,
                &mut turn_run_control,
            )
            .await;
        let terminal_control = turn_run_control.as_ref().unwrap_or(&root_scope_control);
        match result {
            Ok(SingleRunOutcome::IdleGoalInactive) => {
                let execution = root_agent_execution.take().ok_or_else(|| {
                    AppRunError::Message(
                        "inactive goal continuation did not retain its preclaimed root execution"
                            .to_string(),
                    )
                })?;
                self.agent_runtime
                    .release_unadmitted_root_continuation(execution)
                    .map_err(AppRunError::Message)?;
                Ok(SingleRunOutcome::IdleGoalInactive)
            }
            Ok(SingleRunOutcome::Turn(summary)) => {
                let completed = Ok(summary);
                if let Some(execution) = root_agent_execution.take() {
                    self.agent_runtime.complete_root(
                        execution,
                        &completed,
                        terminal_control.cause(),
                    );
                }
                Ok(SingleRunOutcome::Turn(
                    completed.expect("completed run result"),
                ))
            }
            Ok(SingleRunOutcome::ControlCompleted) => {
                if let Some(execution) = root_agent_execution.take() {
                    self.agent_runtime
                        .release_unadmitted_root_continuation(execution)
                        .map_err(AppRunError::Message)?;
                }
                Ok(SingleRunOutcome::ControlCompleted)
            }
            Err(error) => {
                classify_run_error(terminal_control, &error);
                let failed = Err(error);
                if let Some(execution) = root_agent_execution.take() {
                    self.agent_runtime
                        .complete_root(execution, &failed, terminal_control.cause());
                }
                Err(failed.expect_err("failed run result"))
            }
        }
    }

    async fn execute_single_run_inner(
        &self,
        mut request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
        root_agent_execution: &mut Option<AgentRuntimeExecution>,
        turn_run_control: &mut Option<RunControl>,
    ) -> Result<SingleRunOutcome, AppRunError> {
        let requires_active_goal = root_agent_execution.is_some();
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
                        .await
                        .map(|()| SingleRunOutcome::ControlCompleted);
                }
            }
        }
        let session_settings = self.session_settings_for_selector(&selector).await?;
        let effective_config = materialize_run_config(
            self.config.clone(),
            session_settings.as_ref(),
            &request.config,
        );
        let should_generate_session_title = matches!(&selector, SessionSelector::New)
            && request
                .title
                .as_deref()
                .is_none_or(is_placeholder_session_title)
            && !request.prompt.trim().is_empty();
        let image_parts = load_image_attachments(&request.cwd, &request.image_paths)?;
        let prepared = prepare_run_turn(&self.workspace, &request)?;
        let existing_fresh_running_turn = if let Some(existing) = session_settings.as_ref() {
            self.store
                .session_repo()
                .fresh_running_turn_for_session(existing.id)
                .await?
        } else {
            None
        };
        if let Some(existing) = session_settings.as_ref()
            && existing_fresh_running_turn.is_some()
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
                .await
                .map(|()| SingleRunOutcome::ControlCompleted);
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
        let turn_config = Arc::new(
            ResolvedTurnConfig::capture(effective_config.clone())
                .map_err(|error| AppRunError::Message(error.to_string()))?
                .with_model_override(&turn_policy.model.id)
                .map_err(|error| AppRunError::Message(error.to_string()))?,
        );
        if request.agent_context.is_none() {
            let child_states = self
                .store
                .session_repo()
                .list_direct_child_run_admission_states(session_context.session.id)
                .await?;
            if let Some(edge) = blocking_direct_child(&child_states, self.store.active_runs()) {
                return Err(AppRunError::Message(format!(
                    "session {} still has active sub-agent {}; wait for it to finish or cancel the agent tree before starting another root turn",
                    session_context.session.id, edge.agent_path
                )));
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
        let root_confirmation = if provided_agent_context.is_none() {
            Some(request.agent_confirmation.clone().ok_or_else(|| {
                AppRunError::Message(
                    "root execution requires a shared permission confirmation channel".to_string(),
                )
            })?)
        } else {
            None
        };
        let protocol_turn_id = crate::protocol::TurnId::new();
        let process_run_lease = self
            .store
            .try_acquire_run_process_lease(session_context.session.id)?;
        let admission = match slash_goal_command.as_ref() {
            Some(GoalSlashCommand::SetObjective(objective)) => {
                self.store
                    .session_repo()
                    .admit_session_turn_with_goal_objective(
                        session_context.session.id,
                        protocol_turn_id,
                        objective,
                    )
                    .await?
            }
            None if requires_active_goal => {
                match self
                    .store
                    .session_repo()
                    .admit_active_goal_continuation_turn(
                        session_context.session.id,
                        protocol_turn_id,
                    )
                    .await?
                {
                    ActiveGoalTurnAdmission::Admitted(snapshot) => Some(snapshot),
                    ActiveGoalTurnAdmission::GoalInactive => {
                        drop(process_run_lease);
                        return Ok(SingleRunOutcome::IdleGoalInactive);
                    }
                    ActiveGoalTurnAdmission::Unavailable => None,
                }
            }
            _ => {
                self.store
                    .session_repo()
                    .admit_session_turn(session_context.session.id, protocol_turn_id)
                    .await?
            }
        };
        let Some(admission) = admission else {
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
        let slash_goal_result =
            matches!(slash_goal_command, Some(GoalSlashCommand::SetObjective(_)))
                .then(|| {
                    admission
                        .goal
                        .as_ref()
                        .map(|goal| ThreadGoalSetResult {
                            goal: goal.goal.clone(),
                        })
                        .ok_or_else(|| {
                            AppRunError::Message(
                                "atomic goal admission did not return its captured goal"
                                    .to_string(),
                            )
                        })
                })
                .transpose()?;
        let admitted_goal = admission.goal.as_ref().map(|goal| {
            crate::agent::goal_steering::GoalSnapshot::capture(goal.goal_id.clone(), &goal.goal)
        });
        let admission_id = admission.admission_id;
        let session_id = session_context.session.id;
        let agent_context = if let Some(context) = provided_agent_context {
            if turn_run_control.is_none() {
                *turn_run_control = Some(request.run_control.clone());
            }
            Some(context)
        } else if let Some(confirmation) = root_confirmation {
            let execution = match self
                .agent_runtime
                .begin_root(
                    &session_context,
                    Arc::clone(&turn_config),
                    confirmation,
                    request.run_control.clone(),
                )
                .await
            {
                Ok(execution) => execution,
                Err(error) => {
                    let result = finish_admitted_run(
                        &self.store,
                        session_id,
                        admission_id,
                        protocol_turn_id,
                        &request.run_control,
                        Err(AppRunError::Message(error)),
                        Ok(()),
                    )
                    .await;
                    drop(process_run_lease);
                    return result.map(SingleRunOutcome::Turn);
                }
            };
            let admitted_turn_control = execution.run_control();
            let context = execution.context.clone();
            *root_agent_execution = Some(execution);
            *turn_run_control = Some(admitted_turn_control);
            Some(context)
        } else {
            None
        };
        request.run_control = turn_run_control.clone().ok_or_else(|| {
            AppRunError::Message("admitted turn did not retain a run control".to_string())
        })?;
        let heartbeat_stop = CancellationToken::new();
        let heartbeat_repo = self.store.session_repo();
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
                let admission_id = heartbeat_admission_id.clone();
                let run_control = heartbeat_run_control.clone();
                let agent_context = heartbeat_agent_context.clone();
                async move {
                    renew_admitted_run_lease_with_terminal_cancel(
                        repo,
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
            config: Arc::clone(&turn_config),
            goal: admitted_goal,
            current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
        });
        let admitted_result: Result<RunSummary, AppRunError> = async {
            if let Some(adoption) = request.session_access_mode_adoption.as_ref() {
                adoption
                    .adopt(session_id, session_context.session.access_mode)
                    .await
                    .map_err(AppRunError::Message)?;
            }
            if let Some(context) = agent_context
                .as_ref()
                .filter(|context| !context.is_sub_agent())
            {
                context
                    .bind_root_turn_owner(admission_id, protocol_turn_id)
                    .map_err(AppRunError::Message)?;
            }
            let active_run = self
                .store
                .active_runs()
                .try_start(session_id, request.run_control.clone())?;
            if let Some(result) = slash_goal_result.as_ref() {
                renderer.render_thread_goal_set(result)?;
            }
            let mut renderer_sink = RendererSink {
                renderer,
                show_reasoning_summary: request.show_reasoning_summary,
            };
            let recorder = NativeHarnessRecorder::start_best_effort_for_turn(
                &self.store,
                Some(session_id),
                self.workspace.root.clone(),
                protocol_turn_id,
            );
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
                if !context.has_model_context() {
                    return Err(AppRunError::Message(
                        "cannot resume a session without a prompt or active canonical model context"
                            .to_string(),
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
                        admission_id,
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
                        run_control: request.run_control.clone(),
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
            admission_id,
            protocol_turn_id,
            &request.run_control,
            admitted_result,
            heartbeat_result,
        )
        .await;
        drop(process_run_lease);
        result.map(SingleRunOutcome::Turn)
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
    ) -> Result<(), AppRunError> {
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
    ) -> Result<(), AppRunError> {
        let sessions = self
            .store
            .session_repo()
            .list_sessions(request.project_id, request.limit)
            .await?;
        renderer.render_session_list(&sessions)?;
        Ok(())
    }

    async fn execute_session_loaded(
        &self,
        request: SessionLoadedRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let loaded = self
            .session_service
            .loaded_sessions(request.project_id, request.limit, request.include_archived)
            .await?;
        renderer.render_loaded_sessions(&loaded)?;
        Ok(())
    }

    async fn execute_session_search(
        &self,
        request: SessionSearchRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_archive(
        &self,
        request: SessionArchiveRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let session = self
            .session_service
            .set_session_archived(request.session_id, request.archived)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&session))?;
        Ok(())
    }

    async fn execute_session_interrupt(
        &self,
        request: SessionInterruptRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let session = self
            .session_service
            .interrupt_running_session(request.session_id)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&session))?;
        Ok(())
    }

    async fn execute_session_goal_get(
        &self,
        request: SessionGoalGetRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_goal_set(
        &self,
        request: SessionGoalSetRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_goal_clear(
        &self,
        request: SessionGoalClearRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_idle_admission(
        &self,
        request: SessionIdleAdmissionRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let admission = self
            .session_service
            .evaluate_idle_turn_admission(request.session_id, request.pending_trigger_turn)
            .await?;
        renderer.render_session_idle_turn_admission(&admission)?;
        Ok(())
    }

    async fn execute_session_settings_update(
        &self,
        request: SessionSettingsUpdateRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let update = if session_settings_request_is_access_only(&request) {
            self.session_service
                .update_root_session_access_mode(
                    request.session_id,
                    request
                        .access_mode
                        .expect("access-only request must contain an access mode"),
                )
                .await?
        } else {
            self.session_service
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
                .await?
        };
        renderer.render_session_list(std::slice::from_ref(&update.session))?;
        Ok(())
    }

    async fn execute_session_title_update(
        &self,
        request: SessionTitleUpdateRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let update = self
            .session_service
            .update_session_title(request.session_id, request.title)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&update.session))?;
        Ok(())
    }

    async fn execute_session_show(
        &self,
        request: SessionShowRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let page = self
            .session_service
            .canonical_history_page(request.session_id, 0, DEFAULT_SESSION_SHOW_LIMIT)
            .await?;
        if page.items.is_empty() {
            return Err(AppRunError::Message(
                "cannot show session because canonical protocol history is empty".to_string(),
            ));
        }
        renderer.render_session_history_page(&page)?;
        Ok(())
    }

    async fn execute_session_history(
        &self,
        request: SessionHistoryRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let page = self
            .session_service
            .canonical_history_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_history_page(&page)?;
        Ok(())
    }

    async fn execute_session_turns(
        &self,
        request: SessionTurnsRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let page = self
            .session_service
            .canonical_turn_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_turn_page(&page)?;
        Ok(())
    }

    async fn execute_session_events(
        &self,
        request: SessionEventsRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let page = self
            .session_service
            .canonical_runtime_event_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_runtime_event_page(&page)?;
        Ok(())
    }

    async fn execute_session_read(
        &self,
        request: SessionReadRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_rejoin(
        &self,
        request: SessionRejoinRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_rollback(
        &self,
        request: SessionRollbackRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_fork(
        &self,
        request: SessionForkRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
        Ok(())
    }

    async fn execute_session_steer(
        &self,
        request: SessionSteerRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
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
    ) -> Result<(), AppRunError> {
        let active_turn_id = self
            .store
            .session_repo()
            .fresh_running_turn_for_session(request.session_id)
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
        Ok(())
    }
}

fn session_settings_request_is_access_only(request: &SessionSettingsUpdateRequest) -> bool {
    request.access_mode.is_some()
        && request.cwd.is_none()
        && request.model.is_none()
        && request.base_url.is_none()
        && !request.reset_model_parameters
        && request.temperature.is_none()
        && request.top_p.is_none()
        && request.top_k.is_none()
        && request.max_output_tokens.is_none()
}

async fn renew_admitted_run_lease_with_terminal_cancel(
    repo: crate::storage::SqliteSessionRepository,
    session_id: crate::session::SessionId,
    admission_id: AdmissionId,
    turn_id: crate::protocol::TurnId,
    run_control: RunControl,
    agent_context: Option<crate::app::AgentRunContext>,
) -> Result<RunAdmissionLeaseRenewalOutcome, crate::error::StorageError> {
    let outcome = repo
        .renew_admitted_run_lease(session_id, admission_id, turn_id)
        .await?;
    match &outcome {
        RunAdmissionLeaseRenewalOutcome::Renewed => {}
        RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
            if terminal.session_status() == SessionStatus::Completed => {}
        RunAdmissionLeaseRenewalOutcome::Terminal(_) => {
            run_control.supersede();
            if let Some(agent_context) = agent_context {
                let _ = agent_context.cancel_for_durable_terminal();
            }
        }
        RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
            run_control.supersede();
        }
    }
    Ok(outcome)
}

fn spawn_run_admission_heartbeat<Renew, RenewFuture>(
    session_id: crate::session::SessionId,
    admission_id: AdmissionId,
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
    admission_id: AdmissionId,
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
                    RunAdmissionLeaseRenewalOutcome::Terminal(_) => return Ok(()),
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
    admission_id: AdmissionId,
    protocol_turn_id: crate::protocol::TurnId,
    run_control: &RunControl,
    admitted_result: Result<RunSummary, AppRunError>,
    heartbeat_result: Result<(), crate::error::StorageError>,
) -> Result<RunSummary, AppRunError> {
    let admitted_result = admitted_result.and_then(|summary| {
        if summary.session_id() != session_id || summary.turn_id() != protocol_turn_id {
            return Err(AppRunError::Message(format!(
                "admitted run summary identity mismatch: expected session {session_id} turn {protocol_turn_id}, got session {} turn {}",
                summary.session_id(),
                summary.turn_id()
            )));
        }
        Ok(summary)
    });
    let admitted_result = match heartbeat_result {
        Ok(()) => admitted_result,
        Err(heartbeat_error) => match admitted_result {
            Ok(summary) => {
                match durable_run_summary_for_turn(store, session_id, protocol_turn_id).await {
                    Ok(Some(durable)) if durable.status() == SessionStatus::Completed => {
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
        .is_ok_and(|summary| summary.status() == SessionStatus::Completed)
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
    admission_id: AdmissionId,
) -> Result<RunSummary, AppRunError> {
    match (settled, released) {
        (result, Ok(_)) => result,
        (Ok(summary), Err(release_error)) if summary.status() == SessionStatus::Completed => {
            eprintln!(
                "warning: durable run {} completed, but admission {admission_id} could not be released: {release_error}",
                summary.session_id()
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
    admission_id: AdmissionId,
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
            admission_id,
        }));
    }
    let terminal = match cancellation_cause {
        Some(RunCancellationCause::Interruption(cause)) => crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
        Some(RunCancellationCause::Failure(message)) => crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Failed { error: message },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
        Some(RunCancellationCause::Superseded) => unreachable!("handled above"),
        None => crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Failed {
                error: error.to_string(),
            },
            final_response_id: None,
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
        .session_repo()
        .durable_terminal_for_turn(session_id, protocol_turn_id)
        .await?;
    Ok(terminal.map(|terminal| RunSummary::from_terminal(session_id, protocol_turn_id, terminal)))
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

fn materialize_run_config(
    base_config: ResolvedConfig,
    session_settings: Option<&SessionRecord>,
    input: &RunConfigInput,
) -> ResolvedConfig {
    match input {
        RunConfigInput::Layered {
            model,
            base_url,
            config_override,
        } => compose_run_effective_config(
            base_config,
            session_settings,
            config_override.clone(),
            model,
            base_url,
        ),
        RunConfigInput::Resolved(config) => config.clone(),
    }
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
            access_mode: Some(crate::config::AccessMode::Default),
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
        && effective.permissions.access_mode == crate::config::AccessMode::Default
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
    let limits = crate::config::ProviderRequestLimits::product_default();
    if image_paths.len() > limits.max_images {
        return Err(AppRunError::Message(format!(
            "too many image attachments: {} provided, maximum is {}",
            image_paths.len(),
            limits.max_images
        )));
    }
    let mut images = Vec::new();
    let mut total_decoded_bytes = 0_u64;
    let mut total_base64_chars = 0_u64;
    for image_path in image_paths {
        let resolved = if image_path.is_absolute() {
            image_path.clone()
        } else {
            cwd.join(image_path)
        };
        let file = fs::File::open(resolved.as_std_path()).map_err(|error| {
            AppRunError::Message(format!("failed to open image `{resolved}`: {error}"))
        })?;
        let metadata = file.metadata().map_err(|error| {
            AppRunError::Message(format!("failed to stat opened image `{resolved}`: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(AppRunError::Message(format!(
                "image attachment `{resolved}` is not a file"
            )));
        }
        if metadata.len() > limits.max_single_image_decoded_bytes {
            return Err(AppRunError::Message(format!(
                "image attachment `{resolved}` is {} bytes; maximum is {} bytes",
                metadata.len(),
                limits.max_single_image_decoded_bytes
            )));
        }
        let mime_type = image_mime_type(&resolved).ok_or_else(|| {
            AppRunError::Message(format!(
                "unsupported image attachment extension for `{resolved}`; supported: png, jpg, jpeg, webp, gif"
            ))
        })?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len().min(limits.max_single_image_decoded_bytes))
                .unwrap_or_default(),
        );
        file.take(limits.max_single_image_decoded_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| {
                AppRunError::Message(format!("failed to read image `{resolved}`: {error}"))
            })?;
        if bytes.len() as u64 > limits.max_single_image_decoded_bytes {
            return Err(AppRunError::Message(format!(
                "image attachment `{resolved}` exceeded the maximum of {} bytes while it was being read",
                limits.max_single_image_decoded_bytes
            )));
        }
        let validated = validate_image_bytes(mime_type, &bytes, limits).map_err(|error| {
            AppRunError::Message(format!("image attachment `{resolved}` is invalid: {error}"))
        })?;
        total_decoded_bytes = total_decoded_bytes
            .checked_add(validated.decoded_bytes)
            .ok_or_else(|| AppRunError::Message("image byte total overflowed".to_string()))?;
        if total_decoded_bytes > limits.max_total_image_decoded_bytes {
            return Err(AppRunError::Message(format!(
                "image attachments total {total_decoded_bytes} decoded bytes; maximum is {} bytes",
                limits.max_total_image_decoded_bytes
            )));
        }
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        total_base64_chars = total_base64_chars
            .checked_add(data_base64.len() as u64)
            .ok_or_else(|| AppRunError::Message("image base64 total overflowed".to_string()))?;
        if total_base64_chars > limits.max_total_image_base64_chars {
            return Err(AppRunError::Message(format!(
                "image attachments total {total_base64_chars} base64 characters; maximum is {}",
                limits.max_total_image_base64_chars
            )));
        }
        images.push(ImagePart {
            source_path: Some(resolved),
            mime_type: validated.mime_type.to_string(),
            data_base64,
            byte_len: validated.decoded_bytes,
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
    let guarded = PathGuard::require_path(workspace, &path, AccessKind::Read).map_err(|error| {
        AppRunError::Message(format!("failed to resolve workflow `{path}`: {error}"))
    })?;
    let file = match PathGuard::open_validated_read_file(&guarded) {
        Ok(file) => file,
        Err(crate::error::WorkspaceError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(AppRunError::Message(format!(
                "failed to open workflow `{path}`: {error}"
            )));
        }
    };
    if !file
        .metadata()
        .map_err(|error| {
            AppRunError::Message(format!("failed to inspect workflow `{path}`: {error}"))
        })?
        .is_file()
    {
        return Err(AppRunError::Message(format!(
            "workflow `{path}` is not a regular file"
        )));
    }
    let mut bytes = Vec::with_capacity(MAX_WORKFLOW_COMMAND_BYTES.saturating_add(1));
    file.take(MAX_WORKFLOW_COMMAND_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            AppRunError::Message(format!("failed to read workflow `{path}`: {error}"))
        })?;
    if bytes.len() > MAX_WORKFLOW_COMMAND_BYTES {
        return Err(AppRunError::Message(format!(
            "workflow `{path}` exceeds the source byte limit {MAX_WORKFLOW_COMMAND_BYTES}"
        )));
    }
    let template = String::from_utf8(bytes).map_err(|error| {
        AppRunError::Message(format!("workflow `{path}` is not valid UTF-8: {error}"))
    })?;
    let expanded_body = expand_workflow_template(&template, args)?;
    let relative = path
        .strip_prefix(workspace.root.as_path())
        .map(|value| value.as_str().replace('\\', "/"))
        .unwrap_or_else(|_| path.as_str().replace('\\', "/"));
    Ok(Some(WorkflowExpansion {
        name: name.to_string(),
        prompt: format!(
            "Reusable workflow command: /{name}\nSource: {relative}\n\nWorkflow instructions:\n{expanded_body}"
        ),
    }))
}

fn expand_workflow_template(template: &str, args: Option<&str>) -> Result<String, AppRunError> {
    let expanded_len = if template.contains(WORKFLOW_ARGUMENT_PLACEHOLDER) {
        let placeholder_count = template
            .match_indices(WORKFLOW_ARGUMENT_PLACEHOLDER)
            .count();
        let removed_bytes = placeholder_count
            .checked_mul(WORKFLOW_ARGUMENT_PLACEHOLDER.len())
            .ok_or_else(workflow_expansion_overflow)?;
        let retained_bytes = template
            .len()
            .checked_sub(removed_bytes)
            .ok_or_else(workflow_expansion_overflow)?;
        let argument_bytes = placeholder_count
            .checked_mul(args.unwrap_or("").len())
            .ok_or_else(workflow_expansion_overflow)?;
        retained_bytes
            .checked_add(argument_bytes)
            .ok_or_else(workflow_expansion_overflow)?
    } else if let Some(args) = args.filter(|value| !value.is_empty()) {
        template
            .len()
            .checked_add("\n\nUser arguments:\n".len())
            .and_then(|size| size.checked_add(args.len()))
            .ok_or_else(workflow_expansion_overflow)?
    } else {
        template.len()
    };
    if expanded_len > MAX_EXPANDED_WORKFLOW_BYTES {
        return Err(AppRunError::Message(format!(
            "expanded workflow exceeds the byte limit {MAX_EXPANDED_WORKFLOW_BYTES}"
        )));
    }

    Ok(if template.contains(WORKFLOW_ARGUMENT_PLACEHOLDER) {
        template.replace(WORKFLOW_ARGUMENT_PLACEHOLDER, args.unwrap_or(""))
    } else if let Some(args) = args.filter(|value| !value.is_empty()) {
        format!("{template}\n\nUser arguments:\n{args}")
    } else {
        template.to_string()
    })
}

fn workflow_expansion_overflow() -> AppRunError {
    AppRunError::Message("expanded workflow byte length overflowed this platform".to_string())
}

fn parse_workflow_invocation(prompt: &str) -> Option<(&str, Option<&str>)> {
    let trimmed = prompt.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let name_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .count();
    if name_len == 0 {
        return None;
    }
    let name = &rest[..name_len];
    let args = rest[name_len..].trim();
    Some((name, (!args.is_empty()).then_some(args)))
}

const REVIEW_PROMPT_FILE_LIST_LIMIT: usize = 200;

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
        let listed = scope
            .changed_files
            .iter()
            .take(REVIEW_PROMPT_FILE_LIST_LIMIT)
            .map(|path| format!("- {}", path))
            .collect::<Vec<_>>()
            .join("\n");
        let omitted = scope
            .changed_files
            .len()
            .saturating_sub(REVIEW_PROMPT_FILE_LIST_LIMIT);
        if omitted == 0 {
            listed
        } else {
            format!(
                "{listed}\n- … {omitted} additional changed file(s) are not embedded here; enumerate the authoritative scope with git before planning the review"
            )
        }
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
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use base64::Engine as _;
    use camino::Utf8PathBuf;

    use crate::config::model::{ProviderApiMode, ReasoningEffort};
    use crate::config::{ProviderMetadataMode, ResolvedConfig, ResolvedTurnConfig};
    use crate::protocol::{ModeKind, ProtocolEventStore};
    use crate::session::{
        AdmissionId, NewSession, ProjectId, ProjectRepository, SessionRepository, ThreadGoalStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(unix)]
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[test]
    fn only_access_mode_updates_use_the_live_root_session_path() {
        let access_only = super::SessionSettingsUpdateRequest {
            session_id: crate::session::SessionId::new(),
            cwd: None,
            model: None,
            base_url: None,
            access_mode: Some(crate::config::AccessMode::AutoReview),
            reset_model_parameters: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_output_tokens: None,
        };
        assert!(super::session_settings_request_is_access_only(&access_only));

        let combined = super::SessionSettingsUpdateRequest {
            model: Some("replacement-model".to_string()),
            ..access_only.clone()
        };
        assert!(!super::session_settings_request_is_access_only(&combined));

        let no_access = super::SessionSettingsUpdateRequest {
            access_mode: None,
            model: Some("replacement-model".to_string()),
            ..access_only
        };
        assert!(!super::session_settings_request_is_access_only(&no_access));
    }

    #[test]
    fn image_attachment_records_bytes_read_from_the_open_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8");
        let path = root.join("sample.png");
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(640_u32.to_be_bytes());
        bytes.extend(480_u32.to_be_bytes());
        std::fs::write(&path, &bytes).expect("image fixture");

        let images = super::load_image_attachments(&root, &[path]).expect("attachment");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].byte_len, bytes.len() as u64);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&images[0].data_base64)
                .expect("base64"),
            bytes
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn workflow_command_link_cannot_ingest_an_external_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("external.md")).expect("utf8 external");
        std::fs::create_dir_all(root.join(".moyai/commands")).expect("commands directory");
        std::fs::write(&external, "EXTERNAL_WORKFLOW_SECRET").expect("external fixture");
        symlink_file(
            external.as_std_path(),
            root.join(".moyai/commands/escape.md").as_std_path(),
        )
        .expect("workflow symlink fixture");
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &root,
            &ResolvedConfig::default(),
        )
        .expect("workspace");

        let error = super::maybe_expand_workflow_command(&workspace, "/escape")
            .expect_err("external workflow link must fail closed");

        assert!(error.to_string().contains("outside the allowed roots"));
        assert!(!error.to_string().contains("EXTERNAL_WORKFLOW_SECRET"));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn workflow_command_directory_link_cannot_escape_the_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let external_commands = Utf8PathBuf::from_path_buf(temp.path().join("external-commands"))
            .expect("utf8 external commands");
        std::fs::create_dir_all(root.join(".moyai")).expect("moyai directory");
        std::fs::create_dir_all(&external_commands).expect("external commands directory");
        std::fs::write(
            external_commands.join("escape.md"),
            "EXTERNAL_DIRECTORY_WORKFLOW_SECRET",
        )
        .expect("external workflow fixture");
        symlink_dir(
            external_commands.as_std_path(),
            root.join(".moyai/commands").as_std_path(),
        )
        .expect("workflow directory symlink fixture");
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &root,
            &ResolvedConfig::default(),
        )
        .expect("workspace");

        let error = super::maybe_expand_workflow_command(&workspace, "/escape")
            .expect_err("external workflow directory link must fail closed");

        assert!(error.to_string().contains("outside the allowed roots"));
        assert!(
            !error
                .to_string()
                .contains("EXTERNAL_DIRECTORY_WORKFLOW_SECRET")
        );
    }

    #[test]
    fn workflow_command_rejects_a_template_above_the_byte_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(root.join(".moyai/commands")).expect("commands directory");
        std::fs::write(
            root.join(".moyai/commands/large.md"),
            vec![b'x'; super::MAX_WORKFLOW_COMMAND_BYTES + 1],
        )
        .expect("large workflow fixture");
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &root,
            &ResolvedConfig::default(),
        )
        .expect("workspace");

        let error = super::maybe_expand_workflow_command(&workspace, "/large")
            .expect_err("oversized workflow must fail closed");

        assert!(error.to_string().contains("byte limit"));
        assert!(error.to_string().contains("16384"));

        std::fs::write(
            root.join(".moyai/commands/exact.md"),
            vec![b'y'; super::MAX_WORKFLOW_COMMAND_BYTES],
        )
        .expect("exact-limit workflow fixture");
        let exact = super::maybe_expand_workflow_command(&workspace, "/exact")
            .expect("exact-limit workflow")
            .expect("workflow expansion");
        assert!(
            exact
                .prompt
                .ends_with(&"y".repeat(super::MAX_WORKFLOW_COMMAND_BYTES))
        );
    }

    #[test]
    fn workflow_command_rejects_argument_amplification_before_replacement() {
        let template = super::WORKFLOW_ARGUMENT_PLACEHOLDER.repeat(9);
        let args = "z".repeat(8 * 1024);

        let error = super::expand_workflow_template(&template, Some(&args))
            .expect_err("amplified workflow must fail before replacement");

        assert!(error.to_string().contains("byte limit 65536"));
    }

    #[test]
    fn workflow_command_preserves_small_argument_expansion_contracts() {
        assert_eq!(
            super::expand_workflow_template("Do {{args}} now.", Some("the task"))
                .expect("placeholder expansion"),
            "Do the task now."
        );
        assert_eq!(
            super::expand_workflow_template("Do the task.", Some("carefully"))
                .expect("argument append"),
            "Do the task.\n\nUser arguments:\ncarefully"
        );
    }

    #[test]
    fn workflow_invocation_parser_borrows_large_arguments_from_the_request() {
        let prompt = format!(
            "/review {}",
            "x".repeat(super::MAX_EXPANDED_WORKFLOW_BYTES + 1)
        );

        let (name, args) = super::parse_workflow_invocation(&prompt).expect("workflow invocation");
        let args = args.expect("workflow arguments");
        let prompt_start = prompt.as_ptr() as usize;
        let prompt_end = prompt_start + prompt.len();

        for pointer in [name.as_ptr() as usize, args.as_ptr() as usize] {
            assert!(
                (prompt_start..prompt_end).contains(&pointer),
                "parser must not allocate an owned copy before workflow limits are checked"
            );
        }
    }

    #[test]
    fn image_attachment_rejects_a_sparse_file_above_the_hard_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8");
        let path = root.join("oversized.png");
        let file = std::fs::File::create(&path).expect("image fixture");
        file.set_len(
            crate::config::ProviderRequestLimits::product_default().max_single_image_decoded_bytes
                + 1,
        )
        .expect("sparse length");

        let error = super::load_image_attachments(&root, &[path])
            .expect_err("oversized image must be rejected");

        assert!(error.to_string().contains("maximum"));
    }

    #[test]
    fn image_attachment_rejects_extension_and_magic_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8");
        let path = root.join("mismatch.jpg");
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(1_u32.to_be_bytes());
        bytes.extend(1_u32.to_be_bytes());
        std::fs::write(&path, bytes).expect("image fixture");

        let error = super::load_image_attachments(&root, &[path])
            .expect_err("MIME mismatch must be rejected");

        assert!(error.to_string().contains("declared MIME"));
    }

    #[test]
    fn complete_run_config_preserves_explicit_absence_without_relayering() {
        let mut base = ResolvedConfig::default();
        base.model.api_key_env = Some("STALE_API_KEY".to_string());
        base.model.temperature = Some(0.9);
        base.model.extra_body_json = Some(serde_json::json!({ "stale": true }));

        let mut resolved = ResolvedConfig::default();
        resolved.model.model = "resolved-model".to_string();
        resolved.model.api_key_env = None;
        resolved.model.temperature = None;
        resolved.model.extra_body_json = None;

        let materialized = super::materialize_run_config(
            base,
            None,
            &crate::app::RunConfigInput::Resolved(resolved),
        );

        assert_eq!(materialized.model.model, "resolved-model");
        assert_eq!(materialized.model.api_key_env, None);
        assert_eq!(materialized.model.temperature, None);
        assert_eq!(materialized.model.extra_body_json, None);
    }

    #[test]
    fn direct_child_blocker_combines_one_sql_snapshot_with_process_local_runs() {
        let root_session_id = crate::session::SessionId::new();
        let child_session_id = crate::session::SessionId::new();
        let mut states = vec![crate::storage::session_repo::DirectChildRunAdmissionState {
            edge: crate::session::SessionSpawnEdge {
                root_session_id,
                parent_session_id: root_session_id,
                child_session_id,
                agent_path: "/root/worker".to_string(),
                task_name: "worker".to_string(),
                created_at_ms: 1,
            },
            blocks_new_root_turn: false,
        }];
        let active_runs = crate::runtime::ActiveRunRegistry::default();
        assert!(super::blocking_direct_child(&states, &active_runs).is_none());

        let local_lease = active_runs
            .try_start(child_session_id, crate::runtime::RunControl::new())
            .expect("local child run");
        assert_eq!(
            super::blocking_direct_child(&states, &active_runs)
                .expect("local blocker")
                .child_session_id,
            child_session_id
        );
        drop(local_lease);

        states[0].blocks_new_root_turn = true;
        assert_eq!(
            super::blocking_direct_child(&states, &active_runs)
                .expect("durable blocker")
                .child_session_id,
            child_session_id
        );
    }

    #[test]
    fn review_prompt_bounds_file_inventory_and_requires_authoritative_enumeration() {
        let scope = crate::workspace::ReviewScope {
            mode: crate::workspace::ReviewScopeMode::Uncommitted,
            base_ref: Some("HEAD".to_string()),
            head_ref: Some("feature/review".to_string()),
            changed_files: (0..=super::REVIEW_PROMPT_FILE_LIST_LIMIT)
                .map(|index| Utf8PathBuf::from(format!("src/file-{index:03}.rs")))
                .collect(),
            summary: "201 files changed".to_string(),
        };

        let prompt = super::build_review_prompt("", &scope);

        assert!(prompt.contains("src/file-000.rs"));
        assert!(prompt.contains("src/file-199.rs"));
        assert!(!prompt.contains("src/file-200.rs"));
        assert!(prompt.contains("1 additional changed file(s) are not embedded"));
        assert!(prompt.contains("enumerate the authoritative scope with git"));
    }

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
        AdmissionId,
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
            .expect("admitted")
            .admission_id;
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
    fn retained_root_scope_supports_many_fresh_successful_continuation_turns() {
        let root_scope = crate::runtime::RunControl::new();
        let (tree, first_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            1,
            root_scope.clone(),
        )
        .expect("agent tree");
        let mut execution = first_execution;
        let mut completed_controls = Vec::new();

        for completed_turn in 0..6 {
            let turn_control = execution.run_control();
            assert!(!turn_control.same_owner(&root_scope));
            assert!(
                completed_controls
                    .iter()
                    .all(|prior: &crate::runtime::RunControl| !prior.same_owner(&turn_control))
            );
            assert!(turn_control.seal_success());
            completed_controls.push(turn_control);
            tree.complete_execution(
                execution,
                crate::runtime::InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("complete root turn");
            if completed_turn == 5 {
                break;
            }
            execution = match tree
                .try_acquire_root_continuation(root_scope.clone())
                .expect("continuation outcome")
            {
                crate::runtime::AgentRootContinuationOutcome::Admitted(execution) => execution,
                crate::runtime::AgentRootContinuationOutcome::Blocked
                | crate::runtime::AgentRootContinuationOutcome::NotReady
                | crate::runtime::AgentRootContinuationOutcome::Invalid => {
                    panic!("retained root task scope rejected continuation turn {completed_turn}")
                }
            };
        }

        assert_eq!(completed_controls.len(), 6);
        assert_eq!(root_scope.cause(), None);
        assert!(!root_scope.success_is_sealed());
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
                    config: crate::app::RunConfigInput::Layered {
                        model: String::new(),
                        base_url: String::new(),
                        config_override: None,
                    },
                    output_mode: crate::cli::OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: run_control.clone(),
                    session_access_mode_adoption: None,
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
    async fn inactive_goal_ends_preclaimed_continuation_without_admission_or_failure() {
        let config = ResolvedConfig::default();
        let (run_service, store, workspace) = run_service_fixture(config.clone()).await;
        let session = run_service
            .session_service
            .start_or_resume(
                crate::session::SessionStartRequest {
                    selector: crate::session::SessionSelector::New,
                    title: Some("inactive goal continuation".to_string()),
                    cwd: workspace.cwd.clone(),
                    model: config.model.model.clone(),
                    base_url: config.model.base_url.clone(),
                    access_mode: config.permissions.access_mode,
                },
                workspace.clone(),
            )
            .await
            .expect("session");
        let root_scope = crate::runtime::RunControl::new();
        let confirmation = crate::cli::SharedConfirmationPrompt::new(NoPrompt);
        let first_execution = run_service
            .agent_runtime
            .begin_root(
                &session,
                Arc::new(
                    ResolvedTurnConfig::capture(config.clone()).expect("valid test turn config"),
                ),
                confirmation.clone(),
                root_scope.clone(),
            )
            .await
            .expect("first root execution");
        let turn_id = crate::protocol::TurnId::new();
        let admission_id = store
            .session_repo()
            .admit_session_turn(session.session.id, turn_id)
            .await
            .expect("durable first-turn admission")
            .expect("first turn admitted")
            .admission_id;
        first_execution
            .context
            .bind_root_turn_owner(admission_id, turn_id)
            .expect("bind durable first-turn owner");
        assert_eq!(
            commit_completed_turn(&store, session.session.id, admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        assert!(first_execution.run_control().seal_success());
        run_service.agent_runtime.complete_root(
            first_execution,
            &Ok(successful_run_summary(session.session.id, turn_id)),
            None,
        );
        let continuation = match run_service
            .agent_runtime
            .begin_root_continuation(
                session.session.id,
                root_scope.clone(),
                Some(confirmation.clone()),
            )
            .expect("continuation claim")
        {
            crate::app::agent_runtime::AgentRuntimeContinuationOutcome::Admitted(execution) => {
                execution
            }
            crate::app::agent_runtime::AgentRuntimeContinuationOutcome::Blocked
            | crate::app::agent_runtime::AgentRuntimeContinuationOutcome::NotReady
            | crate::app::agent_runtime::AgentRuntimeContinuationOutcome::Invalid => {
                panic!("continuation was not admitted")
            }
        };
        let mut renderer = crate::cli::HumanRenderer::new();
        let mut prompt = NoPrompt;
        let outcome = run_service
            .execute_single_run(
                crate::app::RunRequest {
                    prompt: String::new(),
                    session_id: Some(session.session.id),
                    continue_last: false,
                    title: None,
                    cwd: workspace.cwd.clone(),
                    config: crate::app::RunConfigInput::Layered {
                        model: String::new(),
                        base_url: String::new(),
                        config_override: None,
                    },
                    output_mode: crate::cli::OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: root_scope.clone(),
                    session_access_mode_adoption: None,
                    agent_confirmation: Some(confirmation),
                    agent_context: None,
                },
                &mut renderer,
                &mut prompt,
                Some(continuation),
            )
            .await
            .expect("inactive goal outcome");

        assert!(matches!(outcome, super::SingleRunOutcome::IdleGoalInactive));
        assert_eq!(
            store
                .session_repo()
                .get_session(session.session.id)
                .await
                .expect("session after inactive continuation")
                .status,
            crate::session::SessionStatus::Completed
        );
        assert!(
            !store
                .session_repo()
                .has_fresh_run_admission(session.session.id)
                .await
                .expect("no inactive admission")
        );
        assert_eq!(root_scope.cause(), None);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_service
                .agent_runtime
                .wait_for_tree_quiescence(session.session.id),
        )
        .await
        .expect("bounded tree quiescence wait")
        .expect("tree quiescence");
    }

    fn successful_run_summary(
        session_id: crate::session::SessionId,
        turn_id: crate::protocol::TurnId,
    ) -> crate::session::RunSummary {
        crate::session::RunSummary::from_terminal(
            session_id,
            turn_id,
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        )
    }

    #[test]
    fn admission_release_error_cannot_reverse_durable_success() {
        let session_id = crate::session::SessionId::new();
        let summary = successful_run_summary(session_id, crate::protocol::TurnId::new());
        let reconciled = super::reconcile_admitted_run_release(
            Ok(summary),
            Err(crate::error::StorageError::Message(
                "release write failed".to_string(),
            )),
            AdmissionId::new(),
        )
        .expect("durable success remains observable");

        assert_eq!(reconciled.session_id(), session_id);
        assert_eq!(
            reconciled.status(),
            crate::session::SessionStatus::Completed
        );
    }

    #[tokio::test]
    async fn admitted_run_rejects_a_summary_owned_by_another_turn() {
        let (store, session_id, admission_id, admitted_turn_id) =
            heartbeat_active_turn_fixture("wrong summary turn").await;
        let wrong_turn_id = crate::protocol::TurnId::new();
        assert_ne!(wrong_turn_id, admitted_turn_id);
        let run_control = crate::runtime::RunControl::new();
        let error = super::finish_admitted_run(
            &store,
            session_id,
            admission_id,
            admitted_turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, wrong_turn_id)),
            Ok(()),
        )
        .await
        .expect_err("wrong-turn summary must settle the admitted turn as failure");
        assert!(error.to_string().contains("summary identity mismatch"));
        assert_eq!(
            store
                .session_repo()
                .get_session(session_id)
                .await
                .expect("failed admitted session")
                .status,
            crate::session::SessionStatus::Failed
        );
        assert!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, admitted_turn_id)
                .await
                .expect("admitted terminal lookup")
                .is_some()
        );
        assert!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, wrong_turn_id)
                .await
                .expect("wrong terminal lookup")
                .is_none()
        );
    }

    async fn commit_completed_turn(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
        admission_id: AdmissionId,
        turn_id: crate::protocol::TurnId,
    ) -> crate::storage::session_repo::AdmittedTerminalCommit {
        let event = terminal_event(session_id, crate::protocol::TurnTerminalOutcome::Completed);
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
        outcome: crate::protocol::TurnTerminalOutcome,
    ) -> crate::session::RunEvent {
        crate::session::RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome,
                final_response_id: None,
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
                .expect("admitted")
                .admission_id;
            let failure = super::settle_admitted_run_result(
                &store,
                session.id,
                admission_id,
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
        let root_scope_control = crate::runtime::RunControl::new();
        let (tree, root_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            2,
            root_scope_control.clone(),
        )
        .expect("agent tree");
        let root_turn_control = root_execution.run_control();
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
            &root_turn_control,
            &crate::error::AppRunError::Message(
                "provider failed before terminal settlement".to_string(),
            ),
        );

        assert_eq!(root_turn_control.cause(), Some(failure.clone()));
        assert_eq!(root_scope_control.cause(), Some(failure.clone()));
        assert_eq!(sibling_control.cause(), Some(failure));
        assert!(tree.tree_is_cancelled());
        assert!(sibling_control.begin_tool_effect_admission().is_none());
    }

    #[test]
    fn heartbeat_failure_closes_sibling_admission_while_root_effect_is_reserved() {
        let root_scope_control = crate::runtime::RunControl::new();
        let (tree, root_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            2,
            root_scope_control.clone(),
        )
        .expect("agent tree");
        let root_turn_control = root_execution.run_control();
        let (_, sibling_execution) = tree
            .register_child(
                &crate::runtime::AgentPath::root(),
                "sibling",
                crate::session::SessionId::new(),
                None,
            )
            .expect("sibling");
        let sibling_control = sibling_execution.run_control();
        let root_effect = root_turn_control
            .begin_tool_effect_admission()
            .expect("root effect reservation");
        let failure = crate::runtime::RunCancellationCause::Failure(
            "heartbeat failed during root effect admission".to_string(),
        );

        super::record_heartbeat_failure(
            &root_turn_control,
            "heartbeat failed during root effect admission".to_string(),
        );

        assert_eq!(root_turn_control.cause(), None);
        assert_eq!(root_scope_control.cause(), Some(failure.clone()));
        assert_eq!(sibling_control.cause(), Some(failure.clone()));
        assert!(tree.tree_is_cancelled());
        assert!(sibling_control.begin_tool_effect_admission().is_none());

        assert_eq!(root_effect.admit(), Err(failure.clone()));
        assert_eq!(root_turn_control.cause(), Some(failure));
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
            .expect("admitted")
            .admission_id;

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
                    repo.renew_admitted_run_lease(session.id, admission_id, turn_id)
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
            AdmissionId::new(),
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
            crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
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

        assert!(matches!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                session_id,
                admission_id,
                turn_id,
                run_control.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                if terminal.session_status() == crate::session::SessionStatus::Cancelled
        ));
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
            commit_completed_turn(&store, session_id, admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        let run_control = crate::runtime::RunControl::new();

        assert!(matches!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                session_id,
                admission_id,
                turn_id,
                run_control.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                if terminal.session_status() == crate::session::SessionStatus::Completed
        ));
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
                    repo.renew_admitted_run_lease(session_id, admission_id, turn_id)
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

        let completed_event =
            terminal_event(session_id, crate::protocol::TurnTerminalOutcome::Completed);
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    admission_id,
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
            admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, turn_id)),
            heartbeat_result,
        )
        .await
        .expect("graceful heartbeat must not reverse terminal success");
        assert_eq!(completed.status(), crate::session::SessionStatus::Completed);
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
            commit_completed_turn(&store, session_id, admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("durable terminal truth")
                .map(|terminal| terminal.session_status()),
            Some(crate::session::SessionStatus::Completed)
        );
        let completed = super::finish_admitted_run(
            &store,
            session_id,
            admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, turn_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat diagnostic");
        assert_eq!(completed.status(), crate::session::SessionStatus::Completed);
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
            commit_completed_turn(&store, session_id, admission_id, turn_id).await,
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
            admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, turn_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat panic diagnostic");
        assert_eq!(completed.status(), crate::session::SessionStatus::Completed);
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
            admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, turn_id)),
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
                .map(|terminal| terminal.session_status()),
            Some(crate::session::SessionStatus::Failed)
        );
        assert_eq!(
            commit_completed_turn(&store, session_id, admission_id, turn_id).await,
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
            admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id, turn_id)),
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
