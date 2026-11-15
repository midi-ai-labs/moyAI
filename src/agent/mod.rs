//! Thin agent loop boundary shared by CLI, TUI, and Desktop surfaces.

mod compaction;
pub mod context_manager;
pub(crate) mod goal_steering;
pub mod mode;
mod model_compaction;
mod security_review;
mod turn_settlement;
mod turn_tally;
use turn_tally::TurnTally;
pub mod shared;
pub mod step_context;
pub mod turn_context;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::MultiAgentMode;
#[cfg(test)]
use crate::context::context_window::{COMPACTION_USER_MESSAGE_MAX_TOKENS, estimate_text_tokens};
use crate::context::context_window::{
    ContextWindowTokenStatus, bounded_compaction_user_messages, estimate_model_messages_tokens,
};
use crate::context::world_state::WorldState;
use crate::error::AgentError;
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelContentPart,
    ModelMessage, ModelProfile, ModelToolCall, ToolSchema,
};
#[cfg(test)]
use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload};
use crate::protocol::{HistoryItemId, ModelResponseId, ProtocolEventStore, TurnId};
use crate::runtime::{
    CompactionCheckpointBudget, CompactionDiagnostic, CompactionDiagnosticOutcome,
    RunCancelOutcome, RunCancellationCause, RunControl, RunEventSink, SuccessCommitReservation,
};
#[cfg(test)]
use crate::session::ThreadGoalStatus;
use crate::session::{
    DurableTurnTerminal, FinishReason, RequestDiagnosticsPart, RequestMessageDiagnostic,
    RequestToolCallDiagnostic, RequestToolSchemaDiagnostic, RunConfigSnapshot, RunEvent,
    RunMetrics, RunSummary, SessionContext, SessionId, SessionStatus, TokenUsage, ToolCallId,
};
use crate::storage::{
    StoreBundle,
    session_repo::{
        AdmittedRunState, AdmittedTerminalSettlement, ModelResponseWrite, PendingToolCallWrite,
        RunAdmissionLeaseRenewalOutcome,
    },
};
use crate::tool::ToolResult;
use crate::tool::context::{RunMutationFence, ToolServices};
use crate::tool::registry::ToolRegistry;

const TOOL_CANCELLATION_CLEANUP_TIMEOUT: Duration =
    crate::tool::process::MANAGED_PROCESS_CLEANUP_GRACE;
const COMPACTION_ACCURACY_WARNING: &str = "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted.";
// This reserves planning space inside the input window; it is not an output
// target or a generation override. Sampling and output limits remain host-owned.
const COMPACTION_CHECKPOINT_RESERVE_TOKENS: usize = 1_800;
const COMPACTION_PREFIX_SEARCH_MAX_EVALUATIONS: usize = usize::BITS as usize + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionUnitSelection {
    initial_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactionPrefixProjection {
    after_context_tokens: u32,
    projected_request_tokens: u32,
    source_request_tokens: u32,
}

enum CompactionSummaryOutcome {
    Summary {
        text: String,
        prompt_gap: u32,
        diagnostic: CompactionDiagnostic,
    },
    ReasoningOnlySaturation {
        usage: TokenUsage,
        local_request_tokens: u32,
    },
    SourceContextOverflow {
        error: crate::error::LlmError,
        local_request_tokens: u32,
    },
}

#[cfg(test)]
type ProviderApiKeyResolver =
    dyn Fn(Option<&str>) -> Result<Option<String>, crate::error::LlmError> + Send + Sync;

async fn await_tool_cancellation_cleanup<T>(
    cleanup: impl Future<Output = T>,
    grace: Duration,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(grace, cleanup).await
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(
        &self,
        world_state: &WorldState,
        skills_snapshot: &crate::skill::SkillsSnapshot,
        turn: &turn_context::TurnContext,
    ) -> String {
        let mut sections = vec![
            turn.policy.model.base_instructions.trim().to_string(),
            world_state.rendered.clone(),
            crate::skill::render_available_skills_from_snapshot(skills_snapshot),
        ];
        if let Some(instructions) = turn.mode.developer_instructions {
            sections.insert(1, instructions.trim().to_string());
        }
        let config = turn.resolved_config().runtime_config();
        if turn.policy.model.supports_tools && config.mcp.enabled {
            let connections = mcp_connection_catalog(&config.mcp);
            if !connections.is_empty() {
                sections.push(include_str!("../../assets/prompts/mcp_clients.md").replace(
                    "{{connections}}",
                    &serde_json::to_string(&connections).expect("MCP catalog serialization"),
                ));
            }
        }
        sections.join("\n\n")
    }

    fn multi_agent_context_messages(
        &self,
        turn: &turn_context::TurnContext,
        is_sub_agent: bool,
    ) -> Vec<ModelMessage> {
        if !turn.policy.model.supports_tools {
            return Vec::new();
        }
        let Some(multi_agent_mode) = turn.multi_agent_mode() else {
            return Vec::new();
        };
        let max_concurrent_agents = turn
            .resolved_config()
            .runtime_config()
            .multi_agent
            .max_concurrent_agents
            .to_string();
        let usage_hint = if is_sub_agent {
            include_str!("../../assets/prompts/sub_agent.md")
        } else {
            include_str!("../../assets/prompts/multi_agent_root.md")
        }
        .replace("{{max_concurrent_agents}}", &max_concurrent_agents);
        let mode_instructions = match multi_agent_mode {
            MultiAgentMode::ExplicitRequestOnly => {
                include_str!("../../assets/prompts/multi_agent_explicit.md")
            }
            MultiAgentMode::Proactive => {
                include_str!("../../assets/prompts/multi_agent_proactive.md")
            }
        };
        vec![
            ModelMessage::Developer {
                content: usage_hint.trim().to_string(),
            },
            ModelMessage::Developer {
                content: mode_instructions.trim().to_string(),
            },
        ]
    }
}

fn mcp_connection_catalog(config: &crate::config::McpConfig) -> Vec<serde_json::Value> {
    config
        .servers
        .iter()
        .filter(|server| server.enabled)
        .map(|server| {
            let mut value =
                serde_json::json!({"server_id":server.id,"remote_agent":server.remote_agent});
            if let Some(name) = &server.display_name {
                value["display_name"] = serde_json::Value::String(name.clone());
            }
            value
        })
        .collect()
}

#[cfg(test)]
mod mcp_catalog_tests {
    #[test]
    fn model_catalog_maps_stable_peer_ids_to_public_names_without_credentials() {
        let mut config = crate::config::ResolvedConfig::default().mcp;
        let server = &mut config.servers[0];
        server.enabled = true;
        server.id = "hub-device-stable-id".into();
        server.remote_agent = true;
        server.display_name = Some("Win19 (端末19)".into());
        server
            .headers
            .insert("Authorization".into(), "Bearer secret-fixture".into());
        server.trusted_certificate_pem = Some("public-certificate".into());
        let catalog = super::mcp_connection_catalog(&config);
        assert_eq!(
            catalog,
            vec![
                serde_json::json!({"server_id":"hub-device-stable-id","remote_agent":true,"display_name":"Win19 (端末19)"})
            ]
        );
        config.servers[0].display_name = Some("Win20 renamed".into());
        assert_eq!(
            super::mcp_connection_catalog(&config)[0]["server_id"],
            "hub-device-stable-id"
        );
        config.servers[0].enabled = false;
        assert!(super::mcp_connection_catalog(&config).is_empty());
    }
}

pub struct AgentRunRequest {
    pub session: SessionContext,
    pub turn: Arc<turn_context::TurnContext>,
    pub context: context_manager::ContextManager,
    pub run_control: RunControl,
    pub agent_context: Option<crate::app::AgentRunContext>,
    /// Exact identity appended by the admission transaction. Codex compacts
    /// only the prior conversation before the first sample and drains no
    /// pending input ahead of this item.
    pub initial_user_history_item_id: Option<HistoryItemId>,
}

impl AgentRunRequest {
    fn cancel_token(&self) -> CancellationToken {
        self.run_control.token()
    }

    fn admission_id(&self) -> crate::session::AdmissionId {
        self.turn.admission_id
    }

    fn turn_id(&self) -> TurnId {
        self.turn.turn_id
    }

    fn model_profile(&self) -> ModelProfile {
        self.turn
            .policy
            .model
            .transport_profile(self.turn.provider_target().profile())
    }

    fn model_name(&self) -> &str {
        &self.turn.policy.model.id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MailboxDeliveryPhase {
    CurrentTurnAll,
    NextTurn,
}

impl MailboxDeliveryPhase {
    fn after_model_tool_call(self) -> Self {
        // Codex reopens current-turn mailbox delivery after every tool call.
        Self::CurrentTurnAll
    }
}

#[derive(Clone)]
pub struct AgentLoop {
    llm: Arc<dyn LlmClient>,
    hub_route: Option<crate::hub::HubTurnRoute>,
    registry: ToolRegistry,
    store: StoreBundle,
    prompt_builder: PromptBuilder,
    tool_services: Arc<Mutex<ToolServices>>,
    managed_shell_lifetime: Option<CancellationToken>,
    managed_shell_scope_id: Option<ulid::Ulid>,
    model_request_gate: Option<Arc<tokio::sync::Semaphore>>,
    shared_run: Option<shared::SharedRunContext>,
    #[cfg(test)]
    provider_api_key_resolver: Option<Arc<ProviderApiKeyResolver>>,
    #[cfg(test)]
    before_success_terminal_commit: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl AgentLoop {
    pub(crate) fn managed_shells(&self) -> crate::tool::shell::ManagedShells {
        let shells = self
            .tool_services
            .lock()
            .expect("tool services poisoned")
            .managed_shells
            .clone();
        self.scope_managed_shells(shells)
    }

    fn scope_managed_shells(
        &self,
        shells: crate::tool::shell::ManagedShells,
    ) -> crate::tool::shell::ManagedShells {
        match &self.managed_shell_lifetime {
            Some(lifetime) => shells.with_lifetime(
                lifetime.clone(),
                self.managed_shell_scope_id
                    .expect("scoped managed lifetime"),
            ),
            None => shells,
        }
    }
    pub(crate) fn with_managed_shell_lifetime(
        mut self,
        lifetime: CancellationToken,
        scope_id: ulid::Ulid,
    ) -> Self {
        self.managed_shell_lifetime = Some(lifetime);
        self.managed_shell_scope_id = Some(scope_id);
        self
    }

    pub(crate) fn with_hub_turn(mut self, route: crate::hub::HubTurnRoute) -> Self {
        self.llm = route.client();
        self.hub_route = Some(route);
        self
    }

    pub fn new(
        llm: Arc<dyn LlmClient>,
        registry: ToolRegistry,
        store: StoreBundle,
        prompt_builder: PromptBuilder,
        tool_services: ToolServices,
    ) -> Self {
        Self {
            llm,
            hub_route: None,
            registry,
            store,
            prompt_builder,
            tool_services: Arc::new(Mutex::new(tool_services)),
            managed_shell_lifetime: None,
            managed_shell_scope_id: None,
            model_request_gate: None,
            shared_run: None,
            #[cfg(test)]
            provider_api_key_resolver: None,
            #[cfg(test)]
            before_success_terminal_commit: None,
        }
    }

    fn resolve_provider_api_key(
        &self,
        env_name: Option<&str>,
    ) -> Result<Option<String>, crate::error::LlmError> {
        #[cfg(test)]
        if let Some(resolver) = &self.provider_api_key_resolver {
            return resolver(env_name);
        }
        crate::llm::resolve_api_key_from_env(env_name)
    }

    fn attach_provider_api_key(
        &self,
        request: &AgentRunRequest,
        chat_request: &mut ChatRequest,
    ) -> Result<(), crate::error::LlmError> {
        let model = &request.turn.resolved_config().runtime_config().model;
        chat_request.replace_api_key(self.resolve_provider_api_key(model.api_key_env.as_deref())?);
        Ok(())
    }

    fn guardian_runtime_config(&self, request: &AgentRunRequest) -> crate::config::ResolvedConfig {
        let admitted = request.turn.resolved_config().runtime_config();
        if self
            .hub_route
            .as_ref()
            .is_some_and(|route| !route.approve_configured())
        {
            admitted.clone()
        } else {
            admitted.approve_runtime_config()
        }
    }

    #[cfg(test)]
    fn with_provider_api_key_resolver(mut self, resolver: Arc<ProviderApiKeyResolver>) -> Self {
        self.provider_api_key_resolver = Some(resolver);
        self
    }

    #[cfg(test)]
    fn with_before_success_terminal_commit(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.before_success_terminal_commit = Some(hook);
        self
    }

    pub fn with_model_request_concurrency(mut self, max_concurrent_requests: usize) -> Self {
        self.model_request_gate = Some(Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_requests.max(1),
        )));
        self
    }

    fn tool_services_for_turn(
        &self,
        mcp_config: &crate::config::McpConfig,
    ) -> Result<ToolServices, AgentError> {
        let mut services = self
            .tool_services
            .lock()
            .map_err(|_| AgentError::Message("tool services are unavailable".into()))?;
        if services.mcp.config() != mcp_config {
            services.mcp = Arc::new(crate::mcp::McpClient::new(mcp_config.clone()));
        }
        // Each admitted turn keeps its immutable client, including peer trust
        // and credentials. Equal later configurations reuse its MCP sessions.
        let mut turn_services = services.clone();
        turn_services.managed_shells = self.scope_managed_shells(turn_services.managed_shells);
        Ok(turn_services)
    }

    pub(crate) fn with_shared_run(&self, context: shared::SharedRunContext) -> Self {
        let mut agent = self.clone();
        agent.registry = agent.registry.with_shared_environments(
            &context.project_id,
            &context.job_id,
            &context.attempt_id,
            context.generation,
            &context.environment_id,
            &context.allowed_child_environments,
            &context.allowed_child_candidates,
        );
        agent.shared_run = Some(context);
        agent
    }

    pub async fn run(
        &self,
        request: AgentRunRequest,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        if self.shared_run.is_some() {
            return Err(AgentError::Message(
                "shared execution requires its nonterminal outcome boundary".into(),
            ));
        }
        match self.run_inner(request, prompt, sink).await? {
            shared::AgentRunOutcome::Completed(summary) => Ok(summary),
            shared::AgentRunOutcome::Yielded(_) => {
                unreachable!("ordinary runs cannot yield a shared child")
            }
        }
    }

    pub(crate) async fn run_inner(
        &self,
        mut request: AgentRunRequest,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<shared::AgentRunOutcome, AgentError> {
        ensure_admission_active(&self.store, &request).await?;
        request.context.clear_chat_continuations();
        let tool_services =
            self.tool_services_for_turn(&request.turn.resolved_config().runtime_config().mcp)?;
        let repo = self.store.session_repo();
        if let Some(item_id) = request.initial_user_history_item_id
            && !request.context.contains_user_turn_item(item_id)
        {
            return Err(AgentError::Message(format!(
                "admitted initial UserTurn {item_id} is missing from canonical model context"
            )));
        }

        let progress = self
            .shared_run
            .as_ref()
            .map(|shared| shared.checkpoint())
            .transpose()
            .map_err(AgentError::Message)?
            .flatten()
            .map(|checkpoint| checkpoint.progress)
            .unwrap_or_default();
        let started_at = Instant::now();
        let mut tally = TurnTally::from_progress(progress);
        let goal_snapshot = request.turn.goal().cloned();
        let active_goal_id_for_turn = goal_snapshot
            .as_ref()
            .map(|goal| goal.goal_id().to_string());
        let mut compaction_unavailable_at_revision: Option<String> = None;
        // Live-loop continuation authority only. Stale running turns are
        // terminalized during recovery and must never be inferred from history.
        let mut model_needs_follow_up = self
            .shared_run
            .as_ref()
            .is_some_and(|shared| shared.resume.is_some());
        let mut mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurnAll;
        let mut pre_turn_compaction_item_id = request.initial_user_history_item_id;
        let mut can_drain_pending_input = pre_turn_compaction_item_id.is_none();
        let mut must_sample_without_compaction = false;
        let mut active_context_tokens = context_manager::ActiveContextTokenState::default();

        let outcome: Result<shared::AgentRunOutcome, AgentError> = async {
            'model_round: loop {
                if request.run_control.is_cancelled() {
                    return self
                        .finish_for_cause(
                            &request,
                            &tally,
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }
                ensure_admission_active(&self.store, &request).await?;

                let context_refresh = refresh_committed_context_page(
                    &self.store,
                    request.session.session.id,
                    &mut request.context,
                )?;
                if context_refresh.delta.change == context_manager::HistoryChange::Compacted {
                    active_context_tokens.reset_after_compaction();
                    compaction_unavailable_at_revision = None;
                }
                if !context_refresh.delta.steer_item_ids.is_empty() {
                    mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurnAll;
                    continue;
                }
                if context_refresh.has_more {
                    continue;
                }
                let step_config = request.turn.resolved_config().runtime_config();
                let mut step_registry = self.registry.with_config_overlays(step_config);
                if self.shared_run.is_some() {
                    step_registry.remove_legacy_local_team_tools();
                    step_registry.retain_tools(|name| !matches!(name,
                        "wait_remote_tasks" | "spawn_agent" | "send_message"
                        | "followup_task" | "wait_agent" | "interrupt_agent" | "list_agents"
                    ));
                }
                let skills = tool_services
                    .skills
                    .snapshot_for_workspace(&request.session.workspace.root);
                let mut step = step_context::StepContext::capture(
                    Arc::clone(&request.turn),
                    &request.session.workspace,
                    skills,
                )?;
                let tool_plan =
                    crate::tool::spec_plan::ToolSpecPlan::build(&step, &step_registry);
                let access_mode = crate::tool::context::current_permission_access_mode(
                    &self.store,
                    request.session.session.id,
                    request.agent_context.as_ref(),
                )
                .await?;
                step.refresh_world_state_with_project(
                    &request.session.workspace,
                    self.shared_run.as_ref().and_then(|shared| {
                        shared.project_context.as_ref().map(|project| (project, shared.environment_id.as_str()))
                    }),
                    access_mode,
                )?;
                let supports_images = request
                    .turn
                    .policy
                    .model
                    .input_modalities
                    .contains(&crate::llm::model_policy::InputModality::Image);
                let messages = request.context.model_messages(supports_images);

                if let Some(agent) = request.agent_context.as_ref() {
                    agent.set_activity(format!("Preparing model request {}", tally.model_request_count + 1));
                }

                let prepared_request =
                    self.chat_request(&request, &step, &messages, &tool_plan, goal_snapshot.as_ref())?;
                let overflow_margin_tokens = request
                    .turn
                    .resolved_config()
                    .runtime_config()
                    .session
                    .overflow_margin_tokens;

                // Codex runs this phase at most once and before admitting the
                // just-appended explicit UserTurn into compaction. The exact
                // identity comes from the admission transaction; it is never
                // rediscovered from turn shape.
                let pre_turn_item_id = pre_turn_compaction_item_id.take();
                let mut compaction_exclusions = HashSet::new();
                if let Some(item_id) = pre_turn_item_id {
                    compaction_exclusions.insert(item_id);
                    must_sample_without_compaction = true;
                }
                let compaction_request = if pre_turn_item_id.is_some() {
                    let prior_messages = request
                        .context
                        .model_messages_excluding(&compaction_exclusions, supports_images);
                    self.chat_request(
                        &request,
                        &step,
                        &prior_messages,
                        &tool_plan,
                        goal_snapshot.as_ref(),
                    )?
                    .chat_request
                } else {
                    prepared_request.chat_request.clone()
                };
                let compaction_context_status = if pre_turn_item_id.is_some() {
                    ContextWindowTokenStatus::for_request(
                        &compaction_request,
                        overflow_margin_tokens,
                    )
                } else {
                    active_context_tokens.status_for_request(
                        &request.context,
                        &compaction_request,
                        supports_images,
                        overflow_margin_tokens,
                    )
                };
                let should_compact = !must_sample_without_compaction
                    || pre_turn_item_id.is_some();
                let should_compact = should_compact
                    && compaction_context_status.active_context_tokens
                        >= request.turn.policy.model.working_context_token_limit;
                let compaction_suppressed_for_revision =
                    compaction_unavailable_at_revision.as_deref()
                        == Some(request.context.revision().as_str());
                if should_compact && !compaction_suppressed_for_revision {
                    match self
                        .compact_context(
                            &mut request,
                            &compaction_request,
                            &step,
                            &tool_plan,
                            goal_snapshot.as_ref(),
                            pre_turn_item_id,
                            &mut tally.model_request_count,
                            &mut tally.latest_usage,
                            &mut tally.cumulative_token_usage,
                            sink,
                        )
                        .await
                    {
                        Ok(true) => {
                            active_context_tokens.reset_after_compaction();
                            compaction_unavailable_at_revision = None;
                            must_sample_without_compaction = true;
                            if pre_turn_item_id.is_some() {
                                can_drain_pending_input = false;
                            } else {
                                can_drain_pending_input = !model_needs_follow_up;
                            }
                            continue;
                        }
                        Ok(false) if compaction_context_status.token_limit_reached => {
                            return Err(context_limit_error(&compaction_context_status));
                        }
                        Ok(false) => {
                            compaction_unavailable_at_revision =
                                Some(request.context.revision().as_str().to_string());
                            sink.emit(RunEvent::RecoverableRuntimeFeedback {
                                session_id: request.session.session.id,
                                feedback: crate::session::DurableRuntimeFeedback::new(
                                    crate::session::DurableFeedbackSeverity::Warning,
                                    crate::session::DurableFeedbackCategory::Context,
                                    "Semantic compaction was unavailable, so the task continued with the existing history. If the context fills up, start a new task or reduce the request.",
                                ),
                            })?;
                        }
                        Err(error) => {
                            if let AgentError::Llm(llm_error) = &error
                                && let Some(usage) = llm_error.token_usage()
                            {
                                tally.latest_usage = Some(usage.clone());
                            }
                            if compaction_context_status.token_limit_reached {
                                return Err(error);
                            }
                            compaction_unavailable_at_revision =
                                Some(request.context.revision().as_str().to_string());
                            sink.emit(RunEvent::RecoverableRuntimeFeedback {
                                session_id: request.session.session.id,
                                feedback: crate::session::DurableRuntimeFeedback::new(
                                    crate::session::DurableFeedbackSeverity::Warning,
                                    crate::session::DurableFeedbackCategory::Context,
                                    "Semantic compaction was unavailable, so the task continued with the existing history. If the context fills up, start a new task or reduce the request.",
                                ),
                            })?;
                        }
                    }
                }
                if can_drain_pending_input {
                    // Codex drains the turn-local queue only at the safe
                    // request boundary, after any automatic compaction. It
                    // exhausts same-turn steer input before session mailbox
                    // input, then rebuilds the request from committed history.
                    let delivered_steers =
                        commit_pending_turn_steers(&self.store, &request)?;
                    if delivered_steers > 0 {
                        mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurnAll;
                    }
                    let delivered_mail = if mailbox_delivery_phase
                        != MailboxDeliveryPhase::NextTurn
                    {
                        commit_pending_agent_communications(
                            &request,
                            mailbox_delivery_phase,
                        )?
                    } else {
                        0
                    };
                    // This point-in-time input boundary is consumed exactly
                    // once. Input accepted after it waits for the next sample.
                    can_drain_pending_input = false;
                    if delivered_steers > 0 || delivered_mail > 0 {
                        must_sample_without_compaction = true;
                        continue;
                    }
                }
                if !request.context.has_model_context() {
                    return Err(AgentError::Message(format!(
                        "session {} has no model-visible input after its durable pending-input boundary",
                        request.session.session.id,
                    )));
                }
                let model_compaction_source_item_ids = request.context.model_compaction_source_item_ids();
                let context_status = active_context_tokens.status_for_request(
                    &request.context,
                    &prepared_request.chat_request,
                    supports_images,
                    overflow_margin_tokens,
                );
                if context_status.token_limit_reached {
                    return Err(context_limit_error(&context_status));
                }
                sink.emit(RunEvent::WorldStateUpdated {
                    session_id: request.session.session.id,
                    snapshot: prepared_request.world_state.snapshot.clone(),
                    rendered: prepared_request.world_state.rendered.clone(),
                })?;
                if prepared_request.chat_request.pending_system_prompt_tokens.is_none() {
                sink.emit(RunEvent::ModelRequestPrepared {
                    session_id: request.session.session.id,
                    diagnostics: request_diagnostics(
                        &prepared_request.chat_request,
                        request
                            .turn
                            .resolved_config()
                            .runtime_config()
                            .session
                            .overflow_margin_tokens,
                        Some(context_status.clone()),
                    ),
                })?;
                }
                renew_admission_lease(&self.store, &request).await?;
                tally.model_request_count += 1;
                let model_response_id = ModelResponseId::new();
                let sent_request = prepared_request.chat_request.clone();
                let sent_context_item_ids = request.context.active_item_ids();
                let Some((response_result, mut collector)) = self
                    .execute_model_request(
                        &request,
                        sent_request.clone(),
                        model_response_id,
                        sink,
                    )
                    .await?
                else {
                    return self
                        .finish_for_cause(
                            &request,
                            &tally,
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                };
                match &response_result {
                    Ok(response) => tally.cumulative_token_usage.record_response(response.usage.as_ref()),
                    Err(error) => tally.cumulative_token_usage.record_response(error.token_usage()),
                }
                ensure_admission_active(&self.store, &request).await?;
                let response = match response_result {
                    Ok(response) => response,
                    Err(error) => {
                        if let Some(usage) = error.token_usage().cloned() {
                            tally.latest_usage = Some(usage.clone());
                            if let Some(goal) = &goal_snapshot {
                                self.store
                                    .session_repo()
                                    .account_thread_goal_usage_for_goal(
                                        request.session.session.id,
                                        goal_token_delta(Some(&usage)),
                                        Some(goal.goal_id()),
                                    )
                                    .await?;
                            }
                        }
                        return Err(error.into());
                    }
                };
                tally.latest_usage = response.usage.clone();
                if let Some(goal) = &goal_snapshot {
                    self.store
                        .session_repo()
                        .account_thread_goal_usage_for_goal(
                            request.session.session.id,
                            goal_token_delta(response.usage.as_ref()),
                            Some(goal.goal_id()),
                        )
                        .await?;
                }

                if response.finish_reason == FinishReason::Cancelled {
                    return self
                        .finish_for_cause(
                            &request,
                            &tally,
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }
                if request.run_control.is_cancelled() {
                    return self
                        .finish_for_cause(
                            &request,
                            &tally,
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            sink,
                        )
                        .await;
                }

                let has_tool_calls = !collector.tool_calls.is_empty();
                validate_provider_response_terminal(response.finish_reason, has_tool_calls)?;

                if !has_tool_calls && collector.text.trim().is_empty() {
                    return Err(AgentError::Message(format!(
                        "provider returned an empty final response with finish_reason={:?}",
                        response.finish_reason
                    )));
                }

                // Allocate stable sidecar IDs without parsing or normalizing provider payloads.
                // The raw assistant/tool-call transaction must become durable before any
                // transient execution derivation can run.
                let raw_tool_calls = std::mem::take(&mut collector.tool_calls)
                    .into_iter()
                    .map(|call| (ToolCallId::new(), call))
                    .collect::<Vec<_>>();
                let assistant_text_present = match sent_request.provider_target().api_mode() {
                    crate::config::ProviderApiMode::ChatCompletions => {
                        !collector.text.trim().is_empty()
                    }
                    crate::config::ProviderApiMode::Responses => !collector.text.is_empty(),
                };
                let assistant_text = assistant_text_present.then(|| collector.text.clone());
                let assistant_protocol_sequence_no = assistant_text
                    .as_ref()
                    .and_then(|_| sink.reserve_protocol_sequence_no());
                let tool_call_writes = raw_tool_calls
                    .iter()
                    .map(|(id, call)| PendingToolCallWrite {
                        id: *id,
                        model_call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        arguments_json: call.arguments_json.clone(),
                        protocol_sequence_no: sink.reserve_protocol_sequence_no(),
                    })
                    .collect();
                let committed_response_events = repo
                    .record_model_response_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        request.turn_id(),
                        ModelResponseWrite {
                            response_id: model_response_id,
                            assistant_text,
                            assistant_protocol_sequence_no,
                            tool_calls: tool_call_writes,
                        },
                    )
                    .await?;
                for event in committed_response_events {
                    sink.emit_committed(event)?;
                }
                // A terminal may only reference a response after the complete assistant/tool-call
                // bundle is durable. Provider transport success alone does not create canonical
                // response lineage (for example, output-limit and invalid-shape responses fail
                // before this transaction).
                tally.last_model_response_id = Some(model_response_id);
                if has_tool_calls
                    && let Some(continuation) = response.continuation.clone()
                {
                    request
                        .context
                        .record_chat_continuation(model_response_id, continuation);
                }
                model_needs_follow_up = false;
                must_sample_without_compaction = false;
                can_drain_pending_input = false;
                active_context_tokens.record_provider_response(
                    model_response_id,
                    response.usage.as_ref(),
                    &sent_request,
                    &messages,
                    sent_context_item_ids,
                );
                let pending_turn_steer_after_response = repo
                    .has_pending_turn_steers_for_admitted_turn(
                        request.session.session.id,
                        request.admission_id(),
                        request.turn_id(),
                    )?;

                // ToolName routing, JSON decoding, and schema validation are transient
                // execution concerns. They intentionally happen only after the exact raw
                // provider response bundle above has committed successfully.
                let prepared_tool_calls = raw_tool_calls
                    .into_iter()
                    .map(|(id, call)| {
                        prepare_model_tool_call(id, call, tool_plan.model_visible_specs())
                    })
                    .collect::<Vec<_>>();

                if prepared_tool_calls.is_empty() {
                    if pending_turn_steer_after_response {
                        mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurnAll;
                        can_drain_pending_input = true;
                        continue 'model_round;
                    }
                    // Codex leaves mail which arrives after a visible final response in the
                    // session mailbox. It either starts a later triggered turn or is accepted by
                    // a later user/owner-resume turn; it must not silently extend an answer the
                    // user has already seen.
                    mailbox_delivery_phase = MailboxDeliveryPhase::NextTurn;
                    let context_refresh = refresh_committed_context_page(
                        &self.store,
                        request.session.session.id,
                        &mut request.context,
                    )?;
                    if !context_refresh.delta.steer_item_ids.is_empty() {
                        mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurnAll;
                        continue 'model_round;
                    }
                    if context_refresh.has_more
                        || !context_refresh
                            .delta
                            .agent_communication_item_ids
                            .is_empty()
                    {
                        continue;
                    }
                    return self
                        .commit_success_terminal(
                            &request,
                            &tally,
                            started_at,
                            active_goal_id_for_turn.as_deref(),
                            mailbox_delivery_phase == MailboxDeliveryPhase::CurrentTurnAll,
                            sink,
                        )
                        .await;
                }

                mailbox_delivery_phase = mailbox_delivery_phase.after_model_tool_call();
                let retained_service = tool_services
                    .managed_shells
                    .delegable_service(request.session.session.id);
                if let Some(shared) = self.shared_run.as_ref()
                    && prepared_tool_calls.len() == 1
                    && prepared_tool_calls[0].tool == crate::tool::ToolName::SharedDelegate
                    && prepared_tool_calls[0].validation_error.is_none()
                    && (!tool_services.managed_shells.has_local_work(request.session.session.id)
                        || retained_service.is_some())
                    && !request.run_control.is_cancelled()
                {
                    let call = &prepared_tool_calls[0];
                    if let Ok(child) = crate::tool::shared_delegate::parse_child(
                        call.arguments.clone(), &shared.allowed_child_environments,
                    ) {
                        ensure_admission_active(&self.store, &request).await?;
                        tally.tool_call_count += 1;
                        *tally.tool_calls_by_name.entry("shared_delegate".into()).or_default() += 1;
                        return Ok(shared::AgentRunOutcome::Yielded(shared::SharedYieldProposal {
                            tool_call_id: call.id,
                            child,
                            retained_service,
                            progress: tally.shared_progress(),
                        }));
                    }
                }
                let mut model_compaction_summary = None;
                for call in prepared_tool_calls {
                    if request.run_control.is_cancelled() {
                        return self
                            .finish_for_cause(
                                &request,
                                &tally,
                                started_at,
                                active_goal_id_for_turn.as_deref(),
                                sink,
                            )
                            .await;
                    }
                    ensure_admission_active(&self.store, &request).await?;
                    if let Some(agent) = request.agent_context.as_ref() {
                        agent.set_activity(format!("Running {}", call.call.tool_name));
                    }
                    tally.tool_call_count += 1;
                    *tally.tool_calls_by_name
                        .entry(call.call.tool_name.clone())
                        .or_default() += 1;
                    let tool_output = self
                        .handle_tool_call(
                            tool_plan.router(),
                            &tool_services,
                            &request,
                            call.clone(),
                            &mut tally.model_request_count,
                            &mut tally.cumulative_token_usage,
                            prompt,
                            sink,
                        )
                        .await?;
                    if call.tool == crate::tool::ToolName::CompactContext
                        && matches!(&tool_output, ToolDispatchOutcome::Completed { .. })
                    {
                        model_compaction_summary = Some(
                            crate::tool::compact_context::parse_summary(call.arguments.clone())?
                        );
                    }
                    record_tool_dispatch_failure(
                        &tool_output,
                        &call.call.tool_name,
                        &mut tally.failed_tool_count,
                        &mut tally.failed_tool_calls_by_name,
                    );
                    let (_result_text, tool_change_count) = match tool_output {
                        ToolDispatchOutcome::Completed {
                            result_text,
                            change_count: completed_change_count,
                        } => (result_text, completed_change_count),
                        ToolDispatchOutcome::Declined { result_text } => (result_text, 0),
                        ToolDispatchOutcome::Failed { result_text } => {
                            if let Some(RunCancellationCause::Failure(message)) =
                                request.run_control.cause()
                            {
                                return Err(AgentError::Message(message));
                            }
                            (result_text, 0)
                        }
                        ToolDispatchOutcome::Interrupted {
                            change_count: interrupted_change_count,
                        } => {
                            tally.change_count += interrupted_change_count;
                            return self
                                .finish_for_cause(
                                    &request,
                                    &tally,
                                    started_at,
                                    active_goal_id_for_turn.as_deref(),
                                    sink,
                                )
                                .await;
                        }
                    };
                    ensure_admission_active(&self.store, &request).await?;
                    tally.change_count += tool_change_count;
                }
                if let Some(summary) = model_compaction_summary {
                    loop {
                        let refreshed = refresh_committed_context_page(
                            &self.store, request.session.session.id, &mut request.context,
                        )?;
                        if !refreshed.has_more { break; }
                    }
                    let current_messages = request.context.model_messages(supports_images);
                    let current_template = self.chat_request(
                        &request, &step, &current_messages, &tool_plan, goal_snapshot.as_ref(),
                    )?.chat_request;
                    let current_status = active_context_tokens.status_for_request(
                        &request.context, &current_template, supports_images, overflow_margin_tokens,
                    );
                    if self.apply_model_compaction(
                        &mut request, &current_status, summary,
                        model_compaction_source_item_ids, sink,
                    ).await? {
                        active_context_tokens.reset_after_compaction();
                        compaction_unavailable_at_revision = None;
                    }
                }
                can_drain_pending_input = true;
                model_needs_follow_up = true;
            }
        }
        .await;

        match outcome {
            Ok(summary) => Ok(summary),
            Err(error) => {
                self.settle_run_error(
                    &request,
                    error,
                    &tally,
                    started_at,
                    active_goal_id_for_turn.as_deref(),
                    sink,
                )
                .await
            }
        }
    }

    async fn execute_model_request(
        &self,
        request: &AgentRunRequest,
        mut chat_request: ChatRequest,
        response_id: ModelResponseId,
        sink: &mut dyn RunEventSink,
    ) -> Result<
        Option<(
            Result<LlmResponseSummary, crate::error::LlmError>,
            ResponseCollector,
        )>,
        AgentError,
    > {
        let request_gate = request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.model_request_gate.clone());
        let _model_request_permit = match request_gate {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                let cancel = request.cancel_token();
                tokio::pin!(acquire);
                tokio::select! {
                    permit = &mut acquire => Some(permit.map_err(|_| {
                        AgentError::Message("model request concurrency gate closed".to_string())
                    })?),
                    _ = cancel.cancelled() => None,
                }
            }
            None => None,
        };
        if request.run_control.is_cancelled() {
            return Ok(None);
        }
        self.attach_provider_api_key(request, &mut chat_request)?;

        let mut collector = StreamingResponseCollector::new(response_id, sink);
        collector.late_diagnostics = chat_request.pending_system_prompt_tokens.map(|_| {
            (
                request.session.session.id,
                request
                    .turn
                    .resolved_config()
                    .runtime_config()
                    .session
                    .overflow_margin_tokens,
            )
        });
        let response = {
            let stream = self
                .llm
                .stream_chat(chat_request, request.cancel_token(), &mut collector);
            tokio::pin!(stream);
            loop {
                tokio::select! {
                    response = &mut stream => break Some(response),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if request.run_control.is_cancelled() {
                            break None;
                        }
                        ensure_admission_active(&self.store, request).await?;
                    }
                }
            }
        };
        Ok(response.map(|response| (response, collector.into_inner())))
    }

    fn chat_request(
        &self,
        request: &AgentRunRequest,
        step: &step_context::StepContext,
        messages: &[ModelMessage],
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
        goal: Option<&goal_steering::GoalSnapshot>,
    ) -> Result<PreparedChatRequest, AgentError> {
        let model = request.model_profile();
        let resolved_model = &request.turn.resolved_config().runtime_config().model;
        let provider_target = request.turn.provider_target();
        let is_sub_agent = request
            .agent_context
            .as_ref()
            .is_some_and(crate::app::AgentRunContext::is_sub_agent);
        let system_prompt =
            self.prompt_builder
                .build(&step.world_state, &step.skills, &request.turn);
        let mut request_messages = self
            .prompt_builder
            .multi_agent_context_messages(&request.turn, is_sub_agent);
        request_messages.extend_from_slice(messages);
        if let Some(goal_message) = goal.and_then(goal_steering::steering_message_for_goal) {
            insert_before_final_compaction_summary(&mut request_messages, goal_message);
        }
        let mut chat_request = ChatRequest::new(
            provider_target.clone(),
            model,
            system_prompt,
            request_messages,
            tool_plan.model_visible_specs().to_vec(),
            None,
            crate::config::ProviderReasoningCapability::Unsupported,
            resolved_model.extra_headers.clone(),
        );
        chat_request.parallel_tool_calls = tool_plan.parallel_tool_calls();
        chat_request.pending_system_prompt_tokens = self.llm.pending_system_prompt_tokens();
        chat_request.validate_provider_lifecycle()?;
        Ok(PreparedChatRequest {
            chat_request,
            world_state: step.world_state.clone(),
        })
    }

    async fn compact_context(
        &self,
        request: &mut AgentRunRequest,
        request_template: &ChatRequest,
        step: &step_context::StepContext,
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
        goal: Option<&goal_steering::GoalSnapshot>,
        excluded_item_id: Option<HistoryItemId>,
        model_request_count: &mut usize,
        latest_usage: &mut Option<TokenUsage>,
        cumulative_token_usage: &mut crate::session::CumulativeTokenUsage,
        sink: &mut dyn RunEventSink,
    ) -> Result<bool, AgentError> {
        let excluded_item_ids = excluded_item_id.into_iter().collect::<HashSet<_>>();
        let units = request
            .context
            .automatic_compaction_units_excluding(&excluded_item_ids);
        if units.is_empty() {
            return Ok(false);
        }
        let supports_images = request
            .turn
            .policy
            .model
            .input_modalities
            .contains(&crate::llm::model_policy::InputModality::Image);
        let selection = self.compaction_unit_selection(
            request,
            request_template,
            step,
            tool_plan,
            goal,
            &units,
            &excluded_item_ids,
            supports_images,
        )?;
        let mut selected_unit_count = selection.initial_count;
        let mut retry_available = true;
        let mut required_prompt_gap = 0;
        let (
            selected_item_ids,
            preserved_user_messages,
            summary,
            required_prompt_gap,
            mut diagnostic,
        ) = loop {
            let selected_item_ids = flatten_compaction_unit_prefix(&units, selected_unit_count);
            if selected_item_ids.is_empty() {
                return Ok(false);
            }
            let preserved_user_messages = bounded_compaction_user_messages(
                &request
                    .context
                    .compaction_user_messages_for_items(&selected_item_ids),
            );
            let compaction_source_messages = request.context.compaction_source_messages_for_items(
                &selected_item_ids,
                &excluded_item_ids,
                supports_images,
            );
            let compaction_source_template = self
                .chat_request(request, step, &compaction_source_messages, tool_plan, goal)?
                .chat_request;
            match self
                .summarize_compaction_history(
                    request,
                    &compaction_source_template,
                    model_request_count,
                    latest_usage,
                    cumulative_token_usage,
                    sink,
                )
                .await?
            {
                CompactionSummaryOutcome::Summary {
                    text,
                    prompt_gap,
                    diagnostic,
                } => {
                    required_prompt_gap = required_prompt_gap.max(prompt_gap);
                    let text = request
                        .context
                        .checkpoint_with_source_records(&text, &selected_item_ids)
                        .map_err(|error| {
                            AgentError::Message(format!(
                                "semantic compaction source records could not be preserved: {error}; canonical history was left unchanged"
                            ))
                        })?;
                    break (
                        selected_item_ids,
                        preserved_user_messages,
                        text,
                        required_prompt_gap,
                        diagnostic,
                    );
                }
                CompactionSummaryOutcome::ReasoningOnlySaturation {
                    usage,
                    local_request_tokens,
                } => {
                    let observed_prompt_gap =
                        provider_prompt_gap(Some(&usage), local_request_tokens);
                    if !retry_available {
                        return Err(reasoning_only_compaction_error(
                            &usage,
                            request.turn.policy.model.effective_context_token_limit,
                            observed_prompt_gap,
                        ));
                    }
                    retry_available = false;
                    let Some(retry_count) = self.compaction_retry_unit_count(
                        request,
                        request_template,
                        step,
                        tool_plan,
                        goal,
                        &units,
                        &excluded_item_ids,
                        supports_images,
                        selected_unit_count,
                        local_request_tokens,
                        observed_prompt_gap,
                    )?
                    else {
                        return Err(reasoning_only_compaction_error(
                            &usage,
                            request.turn.policy.model.effective_context_token_limit,
                            observed_prompt_gap,
                        ));
                    };
                    required_prompt_gap = observed_prompt_gap;
                    selected_unit_count = retry_count;
                }
                CompactionSummaryOutcome::SourceContextOverflow {
                    error,
                    local_request_tokens,
                } => {
                    if !retry_available {
                        return Err(AgentError::Llm(error));
                    }
                    retry_available = false;
                    let Some(retry_count) = self.compaction_retry_unit_count(
                        request,
                        request_template,
                        step,
                        tool_plan,
                        goal,
                        &units,
                        &excluded_item_ids,
                        supports_images,
                        selected_unit_count,
                        local_request_tokens,
                        0,
                    )?
                    else {
                        return Err(AgentError::Llm(error));
                    };
                    selected_unit_count = retry_count;
                }
            }
        };
        let before_context_tokens = estimate_model_messages_tokens(
            &request
                .context
                .model_messages_excluding(&excluded_item_ids, supports_images),
        );
        let after_context_tokens =
            estimate_model_messages_tokens(&request.context.model_messages_after_compaction(
                request.session.session.id,
                request.turn_id(),
                preserved_user_messages.clone(),
                summary.clone(),
                selected_item_ids.clone(),
                &excluded_item_ids,
                supports_images,
            ));
        let before_request_tokens =
            ContextWindowTokenStatus::for_request(request_template, 0).active_context_tokens;
        let (checkpoint_budget, usable) = compaction_checkpoint_budget(
            before_context_tokens,
            after_context_tokens,
            before_request_tokens,
            required_prompt_gap,
            request.turn.policy.model.working_context_token_limit,
        );
        diagnostic.checkpoint_budget = Some(checkpoint_budget);
        if !usable {
            diagnostic.outcome = CompactionDiagnosticOutcome::CheckpointRejected;
            sink.record_compaction_diagnostic(&diagnostic);
            // The caller keeps the original history and suppresses another
            // attempt at this revision. A hard-boundary request cannot continue.
            return Ok(false);
        }
        let event = RunEvent::CompactionCompleted {
            layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
            clm_checkpoint: None,

            summarized_messages: selected_item_ids.len(),
            preserved_user_messages,
            summary,
            replacement_item_ids: selected_item_ids,
        };
        if let Err(error) = self
            .store
            .session_repo()
            .commit_admitted_compaction_with_protocol_bundle(
                request.session.session.id,
                request.admission_id(),
                &event,
                request.turn_id(),
                sink.reserve_protocol_sequence_no(),
            )
            .await
        {
            diagnostic.outcome = CompactionDiagnosticOutcome::CommitFailed;
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(error.into());
        }
        diagnostic.outcome = CompactionDiagnosticOutcome::Committed;
        sink.record_compaction_diagnostic(&diagnostic);
        // Canonical compaction is committed by the repository transaction above. A failed
        // publisher/UI projection cannot revoke it; the next loop iteration reloads the durable
        // append through ContextManager.
        let _ = sink.emit_committed(event);
        // Keep this user-facing advisory out of canonical conversation history. Replaying it as
        // an Assistant message would make a resumed provider request look already answered.
        sink.emit_runtime_only(RunEvent::RuntimeNotice {
            session_id: request.session.session.id,
            message: COMPACTION_ACCURACY_WARNING.to_string(),
        })?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn compaction_unit_selection(
        &self,
        request: &AgentRunRequest,
        request_template: &ChatRequest,
        step: &step_context::StepContext,
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
        goal: Option<&goal_steering::GoalSnapshot>,
        units: &[Vec<HistoryItemId>],
        excluded_item_ids: &HashSet<HistoryItemId>,
        supports_images: bool,
    ) -> Result<CompactionUnitSelection, AgentError> {
        debug_assert!(!units.is_empty());
        let source_limit =
            compaction_source_token_limit(request.turn.policy.model.effective_context_token_limit);
        let before_context_tokens = estimate_model_messages_tokens(
            &request
                .context
                .model_messages_excluding(excluded_item_ids, supports_images),
        );
        let before_request_tokens =
            ContextWindowTokenStatus::for_request(request_template, 0).active_context_tokens;
        let checkpoint_reserve = "x".repeat(COMPACTION_CHECKPOINT_RESERVE_TOKENS * 4);
        let actual_source_limit = request
            .turn
            .policy
            .model
            .effective_context_token_limit
            .saturating_sub(COMPACTION_CHECKPOINT_RESERVE_TOKENS as u32);
        let mut projections = HashMap::<usize, CompactionPrefixProjection>::new();
        let mut projection_for = |unit_count| -> Result<CompactionPrefixProjection, AgentError> {
            if let Some(projection) = projections.get(&unit_count) {
                return Ok(*projection);
            }
            let projection = self.compaction_prefix_projection(
                request,
                step,
                tool_plan,
                goal,
                units,
                excluded_item_ids,
                supports_images,
                unit_count,
                before_context_tokens,
                before_request_tokens,
                &checkpoint_reserve,
            )?;
            projections.insert(unit_count, projection);
            Ok(projection)
        };
        let mut initial_count = compaction_prefix_count(
            units.len(),
            source_limit.min(actual_source_limit),
            actual_source_limit,
            &mut |unit_count| {
                projection_for(unit_count).map(|projection| projection.source_request_tokens)
            },
        )?;
        if initial_count > 0 {
            let working_limit = request.turn.policy.model.working_context_token_limit;
            let useful = |projection: CompactionPrefixProjection| {
                compaction_checkpoint_budget(
                    before_context_tokens,
                    projection.after_context_tokens,
                    before_request_tokens,
                    0,
                    working_limit,
                )
                .1
            };
            if !useful(projection_for(initial_count)?) {
                // A small earlier unit can fit the preferred budget without
                // freeing enough room. Advance to the first useful complete
                // prefix when it still fits the actual source-input boundary.
                if let Some(count) =
                    first_monotonic_prefix_count(initial_count + 1, units.len(), &mut |count| {
                        projection_for(count).map(useful)
                    })?
                {
                    if projection_for(count)?.source_request_tokens <= actual_source_limit {
                        initial_count = count;
                    }
                }
            }
        }
        Ok(CompactionUnitSelection { initial_count })
    }

    #[allow(clippy::too_many_arguments)]
    fn compaction_retry_unit_count(
        &self,
        request: &AgentRunRequest,
        request_template: &ChatRequest,
        step: &step_context::StepContext,
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
        goal: Option<&goal_steering::GoalSnapshot>,
        units: &[Vec<HistoryItemId>],
        excluded_item_ids: &HashSet<HistoryItemId>,
        supports_images: bool,
        failed_unit_count: usize,
        failed_source_request_tokens: u32,
        observed_prompt_gap: u32,
    ) -> Result<Option<usize>, AgentError> {
        let Some(max_retry_count) = failed_unit_count.checked_sub(1) else {
            return Ok(None);
        };
        if max_retry_count == 0 {
            return Ok(None);
        }
        let working_limit = request.turn.policy.model.working_context_token_limit;
        let before_context_tokens = estimate_model_messages_tokens(
            &request
                .context
                .model_messages_excluding(excluded_item_ids, supports_images),
        );
        let before_request_tokens =
            ContextWindowTokenStatus::for_request(request_template, 0).active_context_tokens;
        let checkpoint_reserve = "x".repeat(COMPACTION_CHECKPOINT_RESERVE_TOKENS * 4);
        let mut projections = HashMap::<usize, CompactionPrefixProjection>::new();
        let mut projection_for = |unit_count| -> Result<CompactionPrefixProjection, AgentError> {
            if let Some(projection) = projections.get(&unit_count) {
                return Ok(*projection);
            }
            let projection = self.compaction_prefix_projection(
                request,
                step,
                tool_plan,
                goal,
                units,
                excluded_item_ids,
                supports_images,
                unit_count,
                before_context_tokens,
                before_request_tokens,
                &checkpoint_reserve,
            )?;
            projections.insert(unit_count, projection);
            Ok(projection)
        };
        let minimum_gap_safe_count =
            first_monotonic_prefix_count(1, max_retry_count, &mut |unit_count| -> Result<
                bool,
                AgentError,
            > {
                let projection = projection_for(unit_count)?;
                Ok(projection.after_context_tokens < before_context_tokens
                    && projection
                        .projected_request_tokens
                        .saturating_add(observed_prompt_gap)
                        < working_limit)
            })?;
        let largest_source_safe_count = last_monotonic_prefix_count(
            1,
            max_retry_count,
            &mut |unit_count| -> Result<bool, AgentError> {
                let projection = projection_for(unit_count)?;
                Ok(
                    projection.source_request_tokens < failed_source_request_tokens
                        && projection
                            .source_request_tokens
                            .saturating_add(observed_prompt_gap)
                            < working_limit,
                )
            },
        )?;
        let (Some(retry_count), Some(largest_source_safe_count)) =
            (minimum_gap_safe_count, largest_source_safe_count)
        else {
            return Ok(None);
        };
        if retry_count > largest_source_safe_count {
            return Ok(None);
        }
        let projection = projection_for(retry_count)?;
        Ok((projection.after_context_tokens < before_context_tokens
            && projection
                .projected_request_tokens
                .saturating_add(observed_prompt_gap)
                < working_limit
            && projection.source_request_tokens < failed_source_request_tokens
            && projection
                .source_request_tokens
                .saturating_add(observed_prompt_gap)
                < working_limit)
            .then_some(retry_count))
    }

    #[allow(clippy::too_many_arguments)]
    fn compaction_prefix_projection(
        &self,
        request: &AgentRunRequest,
        step: &step_context::StepContext,
        tool_plan: &crate::tool::spec_plan::ToolSpecPlan,
        goal: Option<&goal_steering::GoalSnapshot>,
        units: &[Vec<HistoryItemId>],
        excluded_item_ids: &HashSet<HistoryItemId>,
        supports_images: bool,
        unit_count: usize,
        before_context_tokens: u32,
        before_request_tokens: u32,
        checkpoint_reserve: &str,
    ) -> Result<CompactionPrefixProjection, AgentError> {
        let selected_item_ids = flatten_compaction_unit_prefix(units, unit_count);
        let checkpoint_reserve = request
            .context
            .checkpoint_with_source_records(checkpoint_reserve, &selected_item_ids)
            .map_err(AgentError::Message)?;
        let preserved_user_messages = bounded_compaction_user_messages(
            &request
                .context
                .compaction_user_messages_for_items(&selected_item_ids),
        );
        let after_context_tokens =
            estimate_model_messages_tokens(&request.context.model_messages_after_compaction(
                request.session.session.id,
                request.turn_id(),
                preserved_user_messages,
                checkpoint_reserve,
                selected_item_ids.clone(),
                excluded_item_ids,
                supports_images,
            ));
        let projected_request_tokens = before_request_tokens
            .saturating_sub(before_context_tokens)
            .saturating_add(after_context_tokens);
        let source_messages = request.context.compaction_source_messages_for_items(
            &selected_item_ids,
            excluded_item_ids,
            supports_images,
        );
        let source_template = self
            .chat_request(request, step, &source_messages, tool_plan, goal)?
            .chat_request;
        let source_request =
            compaction_request_with_messages(&source_template, source_template.messages.clone());
        let source_request_tokens =
            ContextWindowTokenStatus::for_request(&source_request, 0).active_context_tokens;
        Ok(CompactionPrefixProjection {
            after_context_tokens,
            projected_request_tokens,
            source_request_tokens,
        })
    }

    async fn summarize_compaction_history(
        &self,
        request: &AgentRunRequest,
        request_template: &ChatRequest,
        model_request_count: &mut usize,
        latest_usage: &mut Option<TokenUsage>,
        cumulative_token_usage: &mut crate::session::CumulativeTokenUsage,
        sink: &mut dyn RunEventSink,
    ) -> Result<CompactionSummaryOutcome, AgentError> {
        let compaction_request =
            compaction_request_with_messages(request_template, request_template.messages.clone());
        let local_request_tokens =
            ContextWindowTokenStatus::for_request(&compaction_request, 0).active_context_tokens;
        match self
            .run_compaction_request(
                request,
                compaction_request,
                model_request_count,
                latest_usage,
                cumulative_token_usage,
                sink,
            )
            .await
        {
            Err(AgentError::Llm(error)) if error.is_context_window_exceeded() => {
                Ok(CompactionSummaryOutcome::SourceContextOverflow {
                    error,
                    local_request_tokens,
                })
            }
            outcome => outcome,
        }
    }

    async fn run_compaction_request(
        &self,
        request: &AgentRunRequest,
        mut compaction_request: ChatRequest,
        model_request_count: &mut usize,
        latest_usage: &mut Option<TokenUsage>,
        cumulative_token_usage: &mut crate::session::CumulativeTokenUsage,
        sink: &mut dyn RunEventSink,
    ) -> Result<CompactionSummaryOutcome, AgentError> {
        let response_id = ModelResponseId::new();
        let local_request_tokens =
            ContextWindowTokenStatus::for_request(&compaction_request, 0).active_context_tokens;
        let mut diagnostic = CompactionDiagnostic {
            response_id,
            provider_request_id: None,
            outcome: CompactionDiagnosticOutcome::PreparationFailed,
            finish_reason: None,
            local_request_tokens,
            usage: None,
            provider_failure_kind: None,
            checkpoint_budget: None,
        };
        if compaction_request.pending_system_prompt_tokens.is_none() {
            sink.emit(RunEvent::ModelRequestPrepared {
                session_id: request.session.session.id,
                diagnostics: request_diagnostics(
                    &compaction_request,
                    request
                        .turn
                        .resolved_config()
                        .runtime_config()
                        .session
                        .overflow_margin_tokens,
                    None,
                ),
            })?;
        }
        if let Err(error) = renew_admission_lease(&self.store, request).await {
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(error);
        }
        *model_request_count += 1;

        let request_gate = request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.model_request_gate.clone());
        let _permit = match request_gate {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                let cancel = request.cancel_token();
                tokio::pin!(acquire);
                tokio::select! {
                    permit = &mut acquire => match permit {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            sink.record_compaction_diagnostic(&diagnostic);
                            return Err(AgentError::Message("model request concurrency gate closed".to_string()));
                        }
                    },
                    _ = cancel.cancelled() => {
                        diagnostic.outcome = CompactionDiagnosticOutcome::Cancelled;
                        sink.record_compaction_diagnostic(&diagnostic);
                        return Err(AgentError::Message("semantic compaction was cancelled".to_string()));
                    },
                }
            }
            None => None,
        };
        if let Err(error) = self.attach_provider_api_key(request, &mut compaction_request) {
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(error.into());
        }

        let effective_context_limit = request.turn.policy.model.effective_context_token_limit;
        let mut collector = CompactionResponseCollector::new(response_id, sink);
        collector.late_diagnostics = compaction_request.pending_system_prompt_tokens.map(|_| {
            (
                request.session.session.id,
                request
                    .turn
                    .resolved_config()
                    .runtime_config()
                    .session
                    .overflow_margin_tokens,
            )
        });
        let response_result = self
            .llm
            .stream_chat(compaction_request, request.cancel_token(), &mut collector)
            .await;
        let collector = collector.into_inner();
        diagnostic.provider_request_id = collector
            .provider_phases
            .last()
            .map(|event| event.request_id.clone());
        let response = match response_result {
            Ok(response) => response,
            Err(error) => {
                diagnostic.outcome = if error.is_context_window_exceeded() {
                    CompactionDiagnosticOutcome::SourceContextOverflow
                } else {
                    CompactionDiagnosticOutcome::ProviderFailed
                };
                diagnostic.usage = error.token_usage().cloned();
                if let Some(failure) = error.provider_failure() {
                    diagnostic.provider_request_id = Some(failure.request_id.clone());
                    diagnostic.provider_failure_kind = Some(failure.kind);
                }
                sink.record_compaction_diagnostic(&diagnostic);
                cumulative_token_usage.record_response(error.token_usage());
                if let Some(usage) = error.token_usage() {
                    *latest_usage = Some(usage.clone());
                    if let Some(goal) = request.turn.goal() {
                        self.store
                            .session_repo()
                            .account_thread_goal_usage_for_goal(
                                request.session.session.id,
                                goal_token_delta(Some(usage)),
                                Some(goal.goal_id()),
                            )
                            .await?;
                    }
                }
                return Err(error.into());
            }
        };
        diagnostic.finish_reason = Some(response.finish_reason);
        diagnostic.usage = response.usage.clone();
        *latest_usage = response.usage.clone();
        cumulative_token_usage.record_response(response.usage.as_ref());
        if let Some(goal) = request.turn.goal() {
            if let Err(error) = self
                .store
                .session_repo()
                .account_thread_goal_usage_for_goal(
                    request.session.session.id,
                    goal_token_delta(response.usage.as_ref()),
                    Some(goal.goal_id()),
                )
                .await
            {
                diagnostic.outcome = CompactionDiagnosticOutcome::UsageAccountingFailed;
                sink.record_compaction_diagnostic(&diagnostic);
                return Err(error.into());
            }
        }
        if response.finish_reason == FinishReason::Cancelled {
            diagnostic.outcome = CompactionDiagnosticOutcome::Cancelled;
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(AgentError::Message(
                "semantic compaction was cancelled; canonical history was left unchanged"
                    .to_string(),
            ));
        }
        if let Err(error) = validate_provider_response_terminal(
            response.finish_reason,
            !collector.tool_calls.is_empty(),
        ) {
            diagnostic.outcome = CompactionDiagnosticOutcome::InvalidFinish;
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(error);
        }
        if !collector.tool_calls.is_empty() {
            diagnostic.outcome = CompactionDiagnosticOutcome::UnexpectedToolCalls;
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(AgentError::Message(
                "semantic compaction returned tool calls instead of a summary".to_string(),
            ));
        }
        let summary = collector.text.trim().to_string();
        if summary.is_empty() {
            if let Some(usage) =
                reasoning_only_saturated_usage(response.usage.as_ref(), effective_context_limit)
            {
                diagnostic.outcome = CompactionDiagnosticOutcome::ReasoningOnlySaturation;
                sink.record_compaction_diagnostic(&diagnostic);
                return Ok(CompactionSummaryOutcome::ReasoningOnlySaturation {
                    usage,
                    local_request_tokens,
                });
            }
            diagnostic.outcome = CompactionDiagnosticOutcome::EmptySummary;
            sink.record_compaction_diagnostic(&diagnostic);
            return Err(AgentError::Message(
                "semantic compaction returned an empty summary".to_string(),
            ));
        }
        compaction::validate_checkpoint_body(&summary).map_err(|error| {
            diagnostic.outcome = CompactionDiagnosticOutcome::InvalidCheckpoint;
            sink.record_compaction_diagnostic(&diagnostic);
            AgentError::Message(format!(
                "semantic compaction checkpoint failed validation: {error}; canonical history was left unchanged"
            ))
        })?;
        Ok(CompactionSummaryOutcome::Summary {
            text: summary,
            prompt_gap: provider_prompt_gap(response.usage.as_ref(), local_request_tokens),
            diagnostic,
        })
    }

    async fn handle_tool_call(
        &self,
        registry: &ToolRegistry,
        tool_services: &ToolServices,
        request: &AgentRunRequest,
        call: PreparedModelToolCall,
        model_request_count: &mut usize,
        cumulative_token_usage: &mut crate::session::CumulativeTokenUsage,
        prompt: &mut dyn ConfirmationPrompt,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let repo = self.store.session_repo();
        let PreparedModelToolCall {
            id: tool_call_id,
            call,
            tool: tool_name,
            arguments,
            validation_error,
        } = call;
        // Classification is host-owned. Invalid calls below never execute.
        let mut metadata = serde_json::json!({"security_effect": "read"});

        if let Some(error_text) = validation_error {
            let result_text = format!("invalid arguments for `{}`: {error_text}", call.tool_name);
            let Some(settlement) = request.run_control.begin_tool_settlement() else {
                return self
                    .settle_pending_tool_for_run_cause(
                        request,
                        tool_call_id,
                        tool_name,
                        metadata,
                        sink,
                    )
                    .await;
            };
            let Some(failed) = repo
                .fail_tool_call_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &result_text,
                    failed_tool_metadata(metadata),
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            else {
                drop(settlement);
                return tool_terminal_race_outcome(request);
            };
            drop(settlement);
            sink.emit_committed(failed)?;
            return Ok(ToolDispatchOutcome::Failed { result_text });
        }

        if let Ok(effect) = registry.validate_call_effect(
            &call.tool_name,
            &arguments,
            &request.turn.resolved_config().runtime_config().mcp,
        ) {
            metadata["security_effect"] = Value::String(effect.as_str().to_owned());
        }
        renew_admission_lease(&self.store, request).await?;
        let mut permission_guardian = AgentPermissionGuardian {
            agent_loop: self,
            request,
            committed_tool_request: &call,
            model_request_count,
            cumulative_token_usage,
            sink,
            pending_retry_lease: None,
            review_retry_lease: None,
            approve_execution_guard: None,
        };
        let ctx = crate::tool::context::ToolContext {
            session: &request.session,
            workspace: &request.session.workspace,
            config: request.turn.resolved_config().runtime_config(),
            tool_call_id,
            cancel: request.cancel_token(),
            run_control: request.run_control.clone(),
            run_mutation_fence: RunMutationFence::new(
                self.store.session_repo(),
                request.session.session.id,
                request.admission_id(),
                request.turn_id(),
                request.run_control.clone(),
            ),
            prompt,
            services: tool_services,
            agent: request.agent_context.as_ref(),
            permission_guardian: Some(&mut permission_guardian),
        };
        let mut cancellation_cleanup_diagnostic = None;
        let execution_result = {
            // Multi-agent mutations cross the in-memory tree and durable session/protocol
            // stores. Once their fenced operation has started, let that short local operation
            // reach a consistent commit or rollback even if this turn is cancelled meanwhile.
            // The ownership check below still prevents recording a result for a stale run.
            let settle_multi_agent_mutation = matches!(
                call.tool_name.as_str(),
                "spawn_agent" | "send_message" | "followup_task" | "interrupt_agent"
            );
            let execution = registry.execute(&call.tool_name, arguments, ctx);
            tokio::pin!(execution);
            loop {
                tokio::select! {
                    result = &mut execution => break result,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if request.run_control.is_cancelled() {
                            break match await_tool_cancellation_cleanup(
                                &mut execution,
                                TOOL_CANCELLATION_CLEANUP_TIMEOUT,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    let message = format!(
                                        "tool cancellation cleanup exceeded {} ms",
                                        TOOL_CANCELLATION_CLEANUP_TIMEOUT.as_millis()
                                    );
                                    cancellation_cleanup_diagnostic = Some(message.clone());
                                    Err(crate::error::ToolError::Message(message))
                                }
                            };
                        }
                        if !settle_multi_agent_mutation {
                            ensure_admission_active(&self.store, request).await?;
                        }
                    }
                }
            }
        };
        drop(permission_guardian);
        let metadata = cancellation_cleanup_diagnostic
            .as_deref()
            .map(|diagnostic| tool_cleanup_diagnostic_metadata(metadata.clone(), diagnostic))
            .unwrap_or(metadata);
        ensure_admission_active(&self.store, request).await?;
        match execution_result {
            Ok(result) => {
                let result_text = tool_result_text(&result);
                let change_count = result.recorded_changes.len();
                let metadata = merge_tool_metadata(metadata, &result);
                if request.run_control.cause().is_some() {
                    return self
                        .settle_executed_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            &result_text,
                            result,
                            sink,
                        )
                        .await;
                }
                let Some(settlement) = request.run_control.begin_tool_settlement() else {
                    return self
                        .settle_executed_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            &result_text,
                            result,
                            sink,
                        )
                        .await;
                };
                if result.change_summaries.is_empty() {
                    let Some(completed) = repo
                        .complete_tool_call_with_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(completed)?;
                } else {
                    let Some((completed, file_changes)) = repo
                        .complete_tool_call_with_file_changes_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            &result.title,
                            metadata,
                            &result_text,
                            result.truncated_output_path.as_deref(),
                            result.change_summaries,
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(completed)?;
                    sink.emit_committed(file_changes)?;
                }
                Ok(ToolDispatchOutcome::Completed {
                    result_text,
                    change_count,
                })
            }
            Err(mut error) => {
                let result_text = error.to_string();
                let permission_denial_settlement = match &mut error {
                    crate::error::ToolError::PermissionDenied { settlement, .. } => {
                        settlement.take()
                    }
                    _ => None,
                };
                if matches!(
                    &error,
                    crate::error::ToolError::PermissionDenied { .. }
                        | crate::error::ToolError::PermissionAborted
                ) {
                    let permission_denied =
                        matches!(&error, crate::error::ToolError::PermissionDenied { .. });
                    let permission_aborted =
                        matches!(&error, crate::error::ToolError::PermissionAborted);
                    let abort_origin_owns = permission_aborted
                        && matches!(
                            request.run_control.cause(),
                            Some(RunCancellationCause::Interruption(
                                crate::protocol::TurnInterruptionCause::ApprovalAborted
                            ))
                        );
                    let settlement = if permission_denied {
                        let Some(settlement) = permission_denial_settlement else {
                            return Err(crate::error::AgentError::Runtime(
                                crate::error::RuntimeError::Message(
                                    "accepted permission denial lost its durable settlement owner"
                                        .to_string(),
                                ),
                            ));
                        };
                        Some(settlement)
                    } else if abort_origin_owns {
                        None
                    } else {
                        let Some(settlement) = request.run_control.begin_tool_settlement() else {
                            return self
                                .settle_pending_tool_for_run_cause(
                                    request,
                                    tool_call_id,
                                    tool_name,
                                    metadata,
                                    sink,
                                )
                                .await;
                        };
                        Some(settlement)
                    };
                    let Some(declined) = repo
                        .settle_tool_call_without_execution_with_protocol_bundle(
                            request.session.session.id,
                            request.admission_id(),
                            tool_call_id,
                            tool_name,
                            crate::session::ToolCallStatus::Declined,
                            &result_text,
                            metadata,
                            request.turn_id(),
                            sink.reserve_protocol_sequence_no(),
                        )
                        .await?
                    else {
                        drop(settlement);
                        return tool_terminal_race_outcome(request);
                    };
                    drop(settlement);
                    sink.emit_committed(declined)?;
                    return Ok(if permission_aborted {
                        ToolDispatchOutcome::Interrupted { change_count: 0 }
                    } else {
                        ToolDispatchOutcome::Declined { result_text }
                    });
                }
                if request.run_control.cause().is_some() {
                    return self
                        .settle_pending_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            sink,
                        )
                        .await;
                }
                let Some(settlement) = request.run_control.begin_tool_settlement() else {
                    return self
                        .settle_pending_tool_for_run_cause(
                            request,
                            tool_call_id,
                            tool_name,
                            metadata,
                            sink,
                        )
                        .await;
                };
                let Some(failed) = repo
                    .fail_tool_call_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        &result_text,
                        failed_tool_metadata(metadata),
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    drop(settlement);
                    return tool_terminal_race_outcome(request);
                };
                drop(settlement);
                sink.emit_committed(failed)?;
                Ok(ToolDispatchOutcome::Failed { result_text })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_pending_tool_for_run_cause(
        &self,
        request: &AgentRunRequest,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        metadata: Value,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let repo = self.store.session_repo();
        match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(interruption)) => {
                let Some(event) = repo
                    .settle_tool_call_without_execution_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        crate::session::ToolCallStatus::Cancelled,
                        interruption.summary(),
                        metadata,
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    return tool_terminal_race_outcome(request);
                };
                sink.emit_committed(event)?;
                Ok(ToolDispatchOutcome::Interrupted { change_count: 0 })
            }
            Some(RunCancellationCause::Failure(message)) => {
                let Some(event) = repo
                    .fail_tool_call_with_protocol_bundle(
                        request.session.session.id,
                        request.admission_id(),
                        tool_call_id,
                        tool_name,
                        &message,
                        metadata,
                        request.turn_id(),
                        sink.reserve_protocol_sequence_no(),
                    )
                    .await?
                else {
                    return tool_terminal_race_outcome(request);
                };
                sink.emit_committed(event)?;
                Ok(ToolDispatchOutcome::Failed {
                    result_text: message,
                })
            }
            Some(RunCancellationCause::Superseded) | None => tool_terminal_race_outcome(request),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_executed_tool_for_run_cause(
        &self,
        request: &AgentRunRequest,
        tool_call_id: ToolCallId,
        tool_name: crate::tool::ToolName,
        metadata: Value,
        result_text: &str,
        result: ToolResult,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolDispatchOutcome, AgentError> {
        let change_count = result.recorded_changes.len();
        let (status, reason, interrupted) = match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(interruption)) => (
                crate::session::ToolCallStatus::Cancelled,
                interruption.summary().to_string(),
                true,
            ),
            Some(RunCancellationCause::Failure(message)) => {
                (crate::session::ToolCallStatus::Failed, message, false)
            }
            Some(RunCancellationCause::Superseded) | None => {
                return tool_terminal_race_outcome(request);
            }
        };
        let repo = self.store.session_repo();
        if result.change_summaries.is_empty() {
            let terminal = if status == crate::session::ToolCallStatus::Cancelled {
                repo.settle_tool_call_without_execution_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    status,
                    &reason,
                    metadata,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            } else {
                repo.fail_tool_call_with_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &reason,
                    metadata,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            };
            let Some(terminal) = terminal else {
                return tool_terminal_race_outcome(request);
            };
            sink.emit_committed(terminal)?;
        } else {
            let Some((terminal, file_changes)) = repo
                .settle_executed_tool_call_with_file_changes_protocol_bundle(
                    request.session.session.id,
                    request.admission_id(),
                    tool_call_id,
                    tool_name,
                    &result.title,
                    metadata,
                    result_text,
                    result.truncated_output_path.as_deref(),
                    status,
                    &reason,
                    result.change_summaries,
                    request.turn_id(),
                    sink.reserve_protocol_sequence_no(),
                    sink.reserve_protocol_sequence_no(),
                )
                .await?
            else {
                return tool_terminal_race_outcome(request);
            };
            sink.emit_committed(terminal)?;
            sink.emit_committed(file_changes)?;
        }
        Ok(if interrupted {
            ToolDispatchOutcome::Interrupted { change_count }
        } else {
            ToolDispatchOutcome::Failed {
                result_text: reason,
            }
        })
    }

    async fn finish_for_run_control_cause(
        &self,
        request: &AgentRunRequest,
        _final_response_id: Option<ModelResponseId>,
        usage: Option<TokenUsage>,
        cumulative_token_usage: &crate::session::CumulativeTokenUsage,
        tool_call_count: usize,
        failed_tool_count: usize,
        change_count: usize,
        model_request_count: usize,
        tool_calls_by_name: BTreeMap<String, usize>,
        failed_tool_calls_by_name: BTreeMap<String, usize>,
        started_at: Instant,
        expected_active_goal_id: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        match request.run_control.cause() {
            Some(RunCancellationCause::Interruption(cause)) => {
                let terminal = DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
                    final_response_id: None,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    metrics: run_metrics(
                        self.hub_route.as_ref(),
                        request,
                        started_at,
                        model_request_count,
                        usage,
                        cumulative_token_usage,
                        &tool_calls_by_name,
                        &failed_tool_calls_by_name,
                    ),
                };
                let terminal_settlement =
                    self.settle_terminal(request, &terminal, None, sink).await?;
                if let Some(effective_terminal) = terminal_settlement.into_terminal() {
                    return Ok(run_summary_from_terminal(request, effective_terminal));
                }
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    return Ok(summary);
                }
                request.run_control.supersede();
                Err(run_superseded_error(request))
            }
            Some(RunCancellationCause::Superseded) => {
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    Ok(summary)
                } else {
                    Err(run_superseded_error(request))
                }
            }
            Some(RunCancellationCause::Failure(message)) => {
                let terminal = DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Failed { error: message },
                    final_response_id: None,
                    tool_call_count,
                    failed_tool_count,
                    change_count,
                    metrics: run_metrics(
                        self.hub_route.as_ref(),
                        request,
                        started_at,
                        model_request_count,
                        usage,
                        cumulative_token_usage,
                        &tool_calls_by_name,
                        &failed_tool_calls_by_name,
                    ),
                };
                let terminal_settlement = self
                    .settle_terminal(request, &terminal, expected_active_goal_id, sink)
                    .await?;
                if let Some(effective_terminal) = terminal_settlement.into_terminal() {
                    return Ok(run_summary_from_terminal(request, effective_terminal));
                }
                if let Some(summary) = self.durable_terminal_summary(request).await? {
                    return Ok(summary);
                }
                request.run_control.supersede();
                Err(run_superseded_error(request))
            }
            None => Err(AgentError::Message(
                "run cancellation was observed without a classified cause".to_string(),
            )),
        }
    }

    async fn durable_terminal_summary(
        &self,
        request: &AgentRunRequest,
    ) -> Result<Option<RunSummary>, AgentError> {
        let terminal = self
            .store
            .session_repo()
            .durable_terminal_for_turn(request.session.session.id, request.turn_id())
            .await?;
        Ok(terminal.map(|terminal| run_summary_from_terminal(request, terminal)))
    }

    async fn settle_terminal(
        &self,
        request: &AgentRunRequest,
        terminal: &DurableTurnTerminal,
        expected_active_goal_id: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<AdmittedTerminalSettlement, AgentError> {
        let event = RunEvent::TurnTerminal {
            session_id: request.session.session.id,
            terminal: Box::new(terminal.clone()),
        };
        let terminal_settlement = self
            .store
            .session_repo()
            .settle_admitted_turn_with_protocol_event(
                request.session.session.id,
                request.admission_id(),
                &event,
                request.turn_id(),
                sink.reserve_protocol_sequence_no(),
                expected_active_goal_id,
            )
            .await?;
        if let AdmittedTerminalSettlement::Applied { terminal } = &terminal_settlement {
            // Durable terminal state has already committed. Projection delivery is best effort
            // and can be recovered from the canonical runtime-event stream.
            let _ = sink.emit_committed(RunEvent::TurnTerminal {
                session_id: request.session.session.id,
                terminal: Box::new(terminal.clone()),
            });
        }
        Ok(terminal_settlement)
    }
}

fn resolve_success_commit_from_durable_summary(
    reservation: SuccessCommitReservation,
    summary: &RunSummary,
) {
    match summary.status() {
        SessionStatus::Completed => {
            reservation.seal();
        }
        SessionStatus::Cancelled => {
            let cause = summary.interruption_cause().map_or(
                RunCancellationCause::Superseded,
                RunCancellationCause::Interruption,
            );
            reservation.resolve_authoritative_cancellation(cause);
        }
        SessionStatus::Failed => {
            reservation.resolve_authoritative_cancellation(RunCancellationCause::Failure(
                "durable failure owner won the terminal commit".to_string(),
            ));
        }
        SessionStatus::Idle | SessionStatus::Running => {
            reservation.resolve_authoritative_cancellation(RunCancellationCause::Superseded);
        }
    }
}

struct PreparedChatRequest {
    chat_request: ChatRequest,
    world_state: WorldState,
}

fn insert_before_final_compaction_summary(
    messages: &mut Vec<ModelMessage>,
    transient: ModelMessage,
) {
    let insertion_index = messages
        .last()
        .is_some_and(context_manager::is_semantic_compaction_message)
        .then_some(messages.len().saturating_sub(1))
        .unwrap_or(messages.len());
    messages.insert(insertion_index, transient);
}

fn first_monotonic_prefix_count<E>(
    mut low: usize,
    mut high: usize,
    predicate: &mut impl FnMut(usize) -> Result<bool, E>,
) -> Result<Option<usize>, E> {
    let mut result = None;
    for _ in 0..COMPACTION_PREFIX_SEARCH_MAX_EVALUATIONS {
        if low > high {
            break;
        }
        let middle = low + (high - low) / 2;
        if predicate(middle)? {
            result = Some(middle);
            if middle == 0 {
                break;
            }
            high = middle - 1;
        } else {
            low = middle.saturating_add(1);
        }
    }
    debug_assert!(low > high || high == usize::MAX);
    Ok(result)
}

fn last_monotonic_prefix_count<E>(
    mut low: usize,
    mut high: usize,
    predicate: &mut impl FnMut(usize) -> Result<bool, E>,
) -> Result<Option<usize>, E> {
    let mut result = None;
    for _ in 0..COMPACTION_PREFIX_SEARCH_MAX_EVALUATIONS {
        if low > high {
            break;
        }
        let middle = low + (high - low) / 2;
        if predicate(middle)? {
            result = Some(middle);
            low = middle.saturating_add(1);
        } else {
            if middle == 0 {
                break;
            }
            high = middle - 1;
        }
    }
    debug_assert!(low > high || high == usize::MAX);
    Ok(result)
}

fn flatten_compaction_unit_prefix(
    units: &[Vec<HistoryItemId>],
    unit_count: usize,
) -> Vec<HistoryItemId> {
    let mut seen_item_ids = HashSet::new();
    units
        .iter()
        .take(unit_count)
        .flatten()
        .copied()
        .filter(|item_id| seen_item_ids.insert(*item_id))
        .collect()
}

fn compaction_prefix_count<E>(
    unit_count: usize,
    preferred_limit: u32,
    actual_limit: u32,
    source_tokens: &mut impl FnMut(usize) -> Result<u32, E>,
) -> Result<usize, E> {
    if unit_count == 0 {
        return Ok(0);
    }
    if source_tokens(unit_count)? <= preferred_limit {
        return Ok(unit_count);
    }
    if let Some(count) =
        last_monotonic_prefix_count(1, unit_count.saturating_sub(1), &mut |count| {
            source_tokens(count).map(|tokens| tokens <= preferred_limit)
        })?
    {
        return Ok(count);
    }
    // A complete first response may exceed preferred decode headroom while
    // still fitting the actual input window. Try it once without splitting it.
    Ok(if source_tokens(1)? <= actual_limit {
        1
    } else {
        0
    })
}

fn compaction_source_token_limit(effective_context_token_limit: u32) -> u32 {
    // Leave room to generate both reasoning and public restart information.
    // This plans the source boundary; it does not set an output target or cap.
    let decode_headroom = (effective_context_token_limit / 5).min(20_000);
    effective_context_token_limit.saturating_sub(decode_headroom)
}

fn compaction_checkpoint_budget(
    before_context_tokens: u32,
    after_context_tokens: u32,
    before_request_tokens: u32,
    observed_prompt_gap: u32,
    working_context_token_limit: u32,
) -> (CompactionCheckpointBudget, bool) {
    let projected_request_tokens = before_request_tokens
        .saturating_sub(before_context_tokens)
        .saturating_add(after_context_tokens);
    let provider_adjusted_projected_request_tokens =
        projected_request_tokens.saturating_add(observed_prompt_gap);
    let usable = after_context_tokens < before_context_tokens
        && provider_adjusted_projected_request_tokens < working_context_token_limit;
    (
        CompactionCheckpointBudget {
            before_context_tokens,
            after_context_tokens,
            before_request_tokens,
            projected_request_tokens,
            provider_adjusted_projected_request_tokens,
            observed_prompt_gap,
            working_context_token_limit,
        },
        usable,
    )
}

fn provider_prompt_gap(usage: Option<&TokenUsage>, local_request_tokens: u32) -> u32 {
    usage
        .map(|usage| usage.prompt_tokens.saturating_sub(local_request_tokens))
        .unwrap_or_default()
}

fn reasoning_only_saturated_usage(
    usage: Option<&TokenUsage>,
    effective_context_limit: u32,
) -> Option<TokenUsage> {
    let usage = usage?;
    (usage.completion_tokens > 0
        && usage.reasoning_tokens == Some(usage.completion_tokens)
        && usage.total_tokens >= effective_context_limit)
        .then(|| usage.clone())
}

fn reasoning_only_compaction_error(
    usage: &TokenUsage,
    effective_context_limit: u32,
    observed_prompt_gap: u32,
) -> AgentError {
    AgentError::Message(format!(
        "semantic compaction returned an empty summary after host-owned reasoning consumed the entire completion near the context limit (prompt {}, completion {}, reasoning {}, total {}, effective input limit {}, observed provider/local prompt gap {}); no smaller safe semantic-unit prefix was available and canonical history was left unchanged",
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.reasoning_tokens.unwrap_or_default(),
        usage.total_tokens,
        effective_context_limit,
        observed_prompt_gap,
    ))
}

fn compaction_request_with_messages(
    template: &ChatRequest,
    mut messages: Vec<ModelMessage>,
) -> ChatRequest {
    let mut request = template.clone();
    // Checkpoints summarize canonical evidence, not private provider reasoning.
    // Unreplaced tool rounds retain their own continuation on normal replay.
    for message in &mut messages {
        if let ModelMessage::AssistantToolCalls { continuation, .. } = message {
            *continuation = None;
        }
    }
    messages.push(ModelMessage::User {
        content: include_str!("../../assets/prompts/compaction.md")
            .trim()
            .to_string(),
    });
    request.messages = messages;
    request.tools.clear();
    request.tool_choice = None;
    request.parallel_tool_calls = false;
    request
}

fn context_limit_error(status: &ContextWindowTokenStatus) -> AgentError {
    AgentError::Message(format!(
        "context window limit reached after semantic compaction was unavailable or insufficient (estimated active context {} tokens, effective full context limit {} tokens, configured overflow margin {} tokens); canonical history was left unchanged",
        status.active_context_tokens,
        status.full_context_window_limit,
        status.overflow_margin_tokens
    ))
}

async fn ensure_admission_active(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<(), AgentError> {
    let state = store
        .session_repo()
        .admitted_run_state(
            request.session.session.id,
            request.turn.admission_id,
            request.turn.turn_id,
        )
        .await?;
    match state {
        AdmittedRunState::OwnedRunning => Ok(()),
        AdmittedRunState::StopFenced(outcome) => {
            classify_run_control_for_terminal_outcome(&request.run_control, &outcome);
            Err(run_superseded_error(request))
        }
        AdmittedRunState::Terminal(_) | AdmittedRunState::SupersededOrExpired => {
            request.run_control.supersede();
            Err(run_superseded_error(request))
        }
    }
}

async fn renew_admission_lease(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<(), AgentError> {
    let renewed = store
        .session_repo()
        .renew_admitted_run_lease(
            request.session.session.id,
            request.turn.admission_id,
            request.turn.turn_id,
        )
        .await?;
    match renewed {
        RunAdmissionLeaseRenewalOutcome::Renewed => Ok(()),
        RunAdmissionLeaseRenewalOutcome::InterruptRequested(cause) => {
            request
                .run_control
                .request_cancel(RunCancellationCause::Interruption(cause));
            Err(run_superseded_error(request))
        }
        RunAdmissionLeaseRenewalOutcome::StopFenced(outcome) => {
            classify_run_control_for_terminal_outcome(&request.run_control, &outcome);
            Err(run_superseded_error(request))
        }
        RunAdmissionLeaseRenewalOutcome::Terminal(_)
        | RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
            request.run_control.supersede();
            Err(run_superseded_error(request))
        }
    }
}

fn classify_run_control_for_terminal_outcome(
    run_control: &RunControl,
    outcome: &crate::protocol::TurnTerminalOutcome,
) {
    match outcome {
        crate::protocol::TurnTerminalOutcome::Completed => {
            run_control.supersede();
        }
        crate::protocol::TurnTerminalOutcome::Interrupted { cause } => {
            run_control.request_cancel(RunCancellationCause::Interruption(*cause));
        }
        crate::protocol::TurnTerminalOutcome::Failed { error } => {
            run_control.request_cancel(RunCancellationCause::Failure(error.clone()));
        }
    }
}

fn run_superseded_error(request: &AgentRunRequest) -> AgentError {
    AgentError::RunSuperseded {
        session_id: request.session.session.id,
        admission_id: request.admission_id(),
    }
}

fn tool_terminal_race_outcome(
    request: &AgentRunRequest,
) -> Result<ToolDispatchOutcome, AgentError> {
    match request.run_control.cause() {
        Some(RunCancellationCause::Interruption(_)) => {
            Ok(ToolDispatchOutcome::Interrupted { change_count: 0 })
        }
        Some(RunCancellationCause::Superseded) => Err(run_superseded_error(request)),
        Some(RunCancellationCause::Failure(message)) => Err(AgentError::Message(message)),
        None => {
            request.run_control.supersede();
            Err(run_superseded_error(request))
        }
    }
}

fn commit_pending_agent_communications(
    request: &AgentRunRequest,
    phase: MailboxDeliveryPhase,
) -> Result<usize, AgentError> {
    let Some(agent) = request.agent_context.as_ref() else {
        return Ok(0);
    };
    let selector = match phase {
        MailboxDeliveryPhase::CurrentTurnAll => {
            crate::storage::session_repo::AgentMailboxDeliverySelector::AllPending
        }
        MailboxDeliveryPhase::NextTurn => return Ok(0),
    };
    let mut delivered = 0usize;
    loop {
        let page = agent
            .commit_pending_mailbox_delivery(selector, crate::protocol::MAX_PROTOCOL_PAGE_LIMIT)
            .map_err(AgentError::Message)?;
        let page_len = page.history_item_ids.len();
        if page.has_more && page_len == 0 {
            return Err(AgentError::Message(format!(
                "durable mailbox delivery for session {} made no progress",
                request.session.session.id,
            )));
        }
        delivered = delivered.saturating_add(page_len);
        if !page.has_more {
            return Ok(delivered);
        }
    }
}

fn commit_pending_turn_steers(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<usize, AgentError> {
    Ok(store
        .session_repo()
        .deliver_all_pending_turn_steers_for_admitted_turn(
            request.session.session.id,
            request.admission_id(),
            request.turn_id(),
        )?
        .len())
}

struct CommittedContextRefreshPage {
    delta: context_manager::ContextDelta,
    has_more: bool,
}

/// Advances the sole durable model-context cursor by at most one bounded page.
///
/// Steer wake-ups intentionally carry no content or item identity. New input is
/// observed exactly once through `ContextManager::append_cursor`, including
/// after a process restart or a missed/coalesced wake-up.
fn refresh_committed_context_page(
    store: &StoreBundle,
    session_id: crate::session::SessionId,
    context: &mut context_manager::ContextManager,
) -> Result<CommittedContextRefreshPage, AgentError> {
    let previous_cursor = context.append_cursor();
    let page = store
        .protocol_event_store()
        .history_item_cursor_page_for_session(
            session_id,
            previous_cursor,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
        )?;
    let has_more = page.has_more();
    if page.items.is_empty() {
        if has_more {
            return Err(AgentError::Message(format!(
                "canonical history cursor for session {session_id} made no progress after {previous_cursor:?}"
            )));
        }
        return Ok(CommittedContextRefreshPage {
            delta: context_manager::ContextDelta {
                change: context_manager::HistoryChange::Unchanged,
                steer_item_ids: Vec::new(),
                agent_communication_item_ids: Vec::new(),
            },
            has_more: false,
        });
    }
    if page.next_cursor == previous_cursor {
        return Err(AgentError::Message(format!(
            "canonical history cursor for session {session_id} did not advance after {previous_cursor:?}"
        )));
    }
    let delta = context.ingest_committed_delta(page.items, page.next_cursor);
    Ok(CommittedContextRefreshPage { delta, has_more })
}

fn validate_provider_response_terminal(
    finish_reason: FinishReason,
    has_tool_calls: bool,
) -> Result<(), AgentError> {
    match (finish_reason, has_tool_calls) {
        (FinishReason::Length, _) => Err(AgentError::ProviderOutputLimit),
        (FinishReason::Error, _) => Err(AgentError::ProviderFinishError),
        (FinishReason::ToolCall, false) | (FinishReason::Stop, true) => {
            Err(AgentError::ProviderFinishShape {
                finish_reason,
                has_tool_calls,
            })
        }
        (FinishReason::Stop, false)
        | (FinishReason::ToolCall, true)
        | (FinishReason::Cancelled, _) => Ok(()),
    }
}

#[derive(Debug, Clone)]
struct PreparedModelToolCall {
    id: ToolCallId,
    call: ModelToolCall,
    tool: crate::tool::ToolName,
    arguments: Value,
    validation_error: Option<String>,
}

fn prepare_model_tool_call(
    id: ToolCallId,
    call: ModelToolCall,
    schemas: &[ToolSchema],
) -> PreparedModelToolCall {
    let tool = crate::tool::ToolName::parse(&call.tool_name);
    let parsed_arguments = parse_tool_arguments(&call.arguments_json)
        .and_then(|value| validate_shallow_schema(&call.tool_name, value, schemas));
    let (arguments, validation_error) = match parsed_arguments {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error.to_string())),
    };
    PreparedModelToolCall {
        id,
        call,
        tool,
        arguments,
        validation_error,
    }
}

enum ToolDispatchOutcome {
    Completed {
        result_text: String,
        change_count: usize,
    },
    Declined {
        result_text: String,
    },
    Failed {
        result_text: String,
    },
    Interrupted {
        change_count: usize,
    },
}

fn record_tool_dispatch_failure(
    outcome: &ToolDispatchOutcome,
    tool_name: &str,
    failed_tool_count: &mut usize,
    failed_tool_calls_by_name: &mut BTreeMap<String, usize>,
) {
    if !matches!(outcome, ToolDispatchOutcome::Failed { .. }) {
        return;
    }
    *failed_tool_count += 1;
    *failed_tool_calls_by_name
        .entry(tool_name.to_string())
        .or_default() += 1;
}

struct AgentPermissionGuardian<'a> {
    agent_loop: &'a AgentLoop,
    request: &'a AgentRunRequest,
    committed_tool_request: &'a ModelToolCall,
    model_request_count: &'a mut usize,
    cumulative_token_usage: &'a mut crate::session::CumulativeTokenUsage,
    sink: &'a mut dyn RunEventSink,
    pending_retry_lease: Option<crate::storage::PermissionReviewLease>,
    review_retry_lease: Option<crate::storage::PermissionReviewLease>,
    approve_execution_guard: Option<crate::hub::HubExecutionGuard>,
}

fn permission_guardian_action_input(
    request: &crate::tool::PermissionRequest,
    evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
    call: &ModelToolCall,
    workspace: &crate::workspace::Workspace,
) -> Result<Value, crate::tool::permission_guardian::PermissionGuardianError> {
    let mut arguments: Value = serde_json::from_str(&call.arguments_json).map_err(|error| {
        crate::tool::permission_guardian::PermissionGuardianError::Request(format!(
            "committed tool arguments cannot be represented as exact action evidence: {error}"
        ))
    })?;
    // These shell schema fields explain the proposal; they do not affect execution.
    // Keep executable strings and nested external-tool arguments unchanged.
    if matches!(call.tool_name.as_str(), "shell" | "shell_start")
        && let Some(fields) = arguments.as_object_mut()
    {
        fields.remove("description");
        fields.remove("justification");
    }
    Ok(serde_json::json!({
        "tool_request": {
            "tool_name": call.tool_name,
            "arguments": arguments,
        },
        "execution_facts": {
            "workspace_root": workspace.authority_root(),
            "access": request.access,
            "outside_workspace": request.outside_workspace,
            "targets": request.targets,
            "risks": request.risks,
            "process_sandbox_after_approval": crate::tool::context::approved_process_sandbox_plan(request.access).audit_description(),
        },
        "action_evidence": evidence,
    }))
}

impl AgentPermissionGuardian<'_> {
    async fn review_inner(
        &mut self,
        permission_request: &crate::tool::PermissionRequest,
        action_evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
    ) -> Result<
        crate::tool::permission_guardian::PermissionGuardianAssessment,
        crate::tool::permission_guardian::PermissionGuardianError,
    > {
        use crate::tool::permission_guardian::PermissionGuardianError;
        let authority =
            permission_guardian_authority_snapshot(&self.agent_loop.store, self.request)
                .await
                .map_err(|reason| {
                    PermissionGuardianError::Request(format!(
                        "latest canonical user authority generation could not be captured: {reason}"
                    ))
                })?;
        let effect_keys = crate::tool::permission_guardian::permission_retry_effect_keys(
            permission_request,
            action_evidence,
            self.committed_tool_request,
            &self.request.session.workspace.root,
        )?;
        let fence_key = crate::storage::PermissionRetryFenceKey::new(
            authority.root_session_id,
            effect_keys.family_version,
            effect_keys.family_sha256,
        )
        .map_err(|error| PermissionGuardianError::Request(error.to_string()))?;
        match self
            .agent_loop
            .store
            .permission_retry_fence_store()
            .begin_review(fence_key, authority.generation, effect_keys.identity_sha256)
            .map_err(|error| PermissionGuardianError::Request(error.to_string()))?
        {
            crate::storage::BeginPermissionReview::Claimed(lease) => {
                self.pending_retry_lease = Some(lease);
            }
            crate::storage::BeginPermissionReview::Blocked(record) => {
                return Err(PermissionGuardianError::RetryFenced(format!(
                    "root session {} already has {:?} review {} for authority generation {} (outcome={:?})",
                    record.key.root_session_id(),
                    record.state,
                    record.review_id,
                    record.authority_history_item_id,
                    record.outcome,
                )));
            }
            crate::storage::BeginPermissionReview::AuthorityChanged {
                current_authority_history_item_id,
            } => {
                return Err(PermissionGuardianError::Request(format!(
                    "canonical user authorization changed before permission review admission (current generation: {current_authority_history_item_id:?})"
                )));
            }
        }

        let approve_route = self
            .agent_loop
            .hub_route
            .as_ref()
            .map(|route| route.begin_approve_review(self.request.cancel_token()))
            .transpose()
            .map_err(|error| PermissionGuardianError::Request(error.to_string()))?
            .flatten();
        let mut guardian_config = self.agent_loop.guardian_runtime_config(self.request);
        let guardian_client = match &approve_route {
            Some(route) => {
                guardian_config = route.runtime_config(&guardian_config);
                self.approve_execution_guard = Some(route.execution_guard());
                route.client()
            }
            None => self.agent_loop.llm.clone(),
        };
        let guardian_target = if approve_route.is_some() {
            crate::config::ProviderTarget::from_resolved_config(&guardian_config)
                .map_err(|error| PermissionGuardianError::Request(error.to_string()))?
        } else if self.agent_loop.hub_route.is_some() {
            self.request.turn.provider_target().clone()
        } else {
            self.request
                .turn
                .resolved_config()
                .approve_provider()
                .clone()
        };
        let _guardian_transport = resolved_guardian_isolation_transport(&guardian_target)
            .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;

        let evidence = permission_guardian_action_input(
            permission_request,
            action_evidence,
            self.committed_tool_request,
            &self.request.session.workspace,
        )?;
        let input = serde_json::to_string_pretty(&evidence)
            .map_err(|error| PermissionGuardianError::Request(error.to_string()))?;
        let model = crate::llm::model_policy::ModelPolicy::from_config(&guardian_config)
            .transport_profile(guardian_target.profile());
        let resolved_model = &guardian_config.model;
        let mut guardian_request = ChatRequest::new(
            guardian_target,
            model,
            include_str!("../../assets/prompts/permission_guardian.md")
                .trim()
                .to_string(),
            vec![ModelMessage::User { content: input }],
            Vec::new(),
            None,
            crate::config::ProviderReasoningCapability::Unsupported,
            resolved_model.extra_headers.clone(),
        );
        guardian_request.pending_system_prompt_tokens =
            guardian_client.pending_system_prompt_tokens();
        guardian_request
            .validate_provider_lifecycle()
            .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;

        if guardian_request.pending_system_prompt_tokens.is_none() {
            self.sink
                .emit(RunEvent::ModelRequestPrepared {
                    session_id: self.request.session.session.id,
                    diagnostics: request_diagnostics(
                        &guardian_request,
                        self.request
                            .turn
                            .resolved_config()
                            .runtime_config()
                            .session
                            .overflow_margin_tokens,
                        None,
                    ),
                })
                .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;
        }
        renew_admission_lease(&self.agent_loop.store, self.request)
            .await
            .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;
        *self.model_request_count += 1;

        let request_gate = self
            .request
            .agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::model_request_gate)
            .or_else(|| self.agent_loop.model_request_gate.clone());
        let _permit = match request_gate {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                let cancel = self.request.cancel_token();
                tokio::pin!(acquire);
                loop {
                    tokio::select! {
                        permit = &mut acquire => {
                            break Some(permit.map_err(|_| PermissionGuardianError::Request(
                                "model request concurrency gate closed".to_string()
                            ))?);
                        }
                        _ = cancel.cancelled() => {
                            return Err(PermissionGuardianError::Cancelled);
                        }
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {
                            ensure_admission_active(&self.agent_loop.store, self.request)
                                .await
                                .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;
                        }
                    }
                }
            }
            None => None,
        };
        guardian_request.replace_api_key(
            self.agent_loop
                .resolve_provider_api_key(resolved_model.api_key_env.as_deref())
                .map_err(|error| PermissionGuardianError::Request(error.to_string()))?,
        );

        let response_id = ModelResponseId::new();
        let guardian_store = self.agent_loop.store.clone();
        let guardian_session_id = self.request.session.session.id;
        let guardian_goal_id = self
            .request
            .turn
            .goal()
            .map(|goal| goal.goal_id().to_string());
        let cancel = self.request.cancel_token();
        let mut collector = CompactionResponseCollector::new(response_id, self.sink);
        collector.late_diagnostics = guardian_request.pending_system_prompt_tokens.map(|_| {
            (
                self.request.session.session.id,
                self.request
                    .turn
                    .resolved_config()
                    .runtime_config()
                    .session
                    .overflow_margin_tokens,
            )
        });
        let response_result = {
            let generation =
                guardian_client.stream_chat(guardian_request, cancel.clone(), &mut collector);
            tokio::pin!(generation);
            loop {
                tokio::select! {
                    response = &mut generation => break response,
                    _ = cancel.cancelled() => {
                        return Err(PermissionGuardianError::Cancelled);
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        ensure_admission_active(&self.agent_loop.store, self.request)
                            .await
                            .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;
                    }
                }
            }
        };
        let response = match response_result {
            Ok(response) => {
                self.cumulative_token_usage
                    .record_response(response.usage.as_ref());
                if response.finish_reason == FinishReason::Cancelled {
                    return Err(PermissionGuardianError::Cancelled);
                }
                account_permission_guardian_goal_usage(
                    &guardian_store,
                    guardian_session_id,
                    guardian_goal_id.as_deref(),
                    response.usage.as_ref(),
                )
                .await?;
                response
            }
            Err(error) => {
                self.cumulative_token_usage
                    .record_response(error.token_usage());
                account_permission_guardian_goal_usage(
                    &guardian_store,
                    guardian_session_id,
                    guardian_goal_id.as_deref(),
                    error.token_usage(),
                )
                .await?;
                return Err(permission_guardian_provider_error(error));
            }
        };
        let collector = collector.into_inner();
        crate::llm::validate_toolless_text_response(
            "automatic permission review",
            &response,
            !collector.tool_calls.is_empty(),
        )
        .map_err(|error| PermissionGuardianError::InvalidDecision(error.public_message()))?;
        ensure_admission_active(&self.agent_loop.store, self.request)
            .await
            .map_err(|error| PermissionGuardianError::UnfencedAdmission(error.to_string()))?;
        crate::tool::permission_guardian::parse_guardian_assessment(&collector.text)
    }
}

#[async_trait::async_trait(?Send)]
impl crate::tool::permission_guardian::PermissionGuardian for AgentPermissionGuardian<'_> {
    async fn review(
        &mut self,
        permission_request: &crate::tool::PermissionRequest,
        action_evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
    ) -> Result<
        crate::tool::permission_guardian::PermissionGuardianAssessment,
        crate::tool::permission_guardian::PermissionGuardianError,
    > {
        use crate::tool::permission_guardian::PermissionGuardianError;
        let cancel = self.request.cancel_token();
        let guardian_config = self.agent_loop.guardian_runtime_config(self.request);
        let total_deadline = permission_guardian_total_deadline(crate::config::ProviderDeadlines {
            request_timeout_ms: guardian_config.model.request_timeout_ms,
            connect_timeout_ms: guardian_config.model.connect_timeout_ms,
            max_connect_retries: guardian_config.model.max_retries,
        });
        let result = permission_guardian_review_with_deadline(
            cancel,
            total_deadline,
            self.review_inner(permission_request, action_evidence),
        )
        .await;

        if let Some(guard) = self.approve_execution_guard.take() {
            guard.finish().await;
        }

        let Some(lease) = self.pending_retry_lease.take() else {
            return match result {
                Err(
                    PermissionGuardianError::Cancelled | PermissionGuardianError::RetryFenced(_),
                ) => result,
                Err(PermissionGuardianError::UnfencedAdmission(_)) => result,
                Err(error) => Err(PermissionGuardianError::UnfencedAdmission(
                    error.to_string(),
                )),
                Ok(_) => Err(PermissionGuardianError::UnfencedAdmission(
                    "Guardian returned a decision without a durable retry-fence claim".to_string(),
                )),
            };
        };
        let fence_outcome = permission_retry_fence_outcome(&result);
        match &result {
            Ok(assessment) => {
                use crate::tool::permission_guardian::PermissionGuardianDecision;
                match assessment.decision() {
                    PermissionGuardianDecision::Allow => {
                        require_owned_permission_fence_transition(
                            lease.mark_allowed_pending(),
                            "recording Guardian Allow",
                        )?;
                        self.review_retry_lease = Some(lease);
                    }
                    PermissionGuardianDecision::AskUser => {
                        self.review_retry_lease = Some(lease);
                    }
                    PermissionGuardianDecision::Deny => {
                        require_owned_permission_fence_transition(
                            lease.mark_denied(fence_outcome.expect("Deny has a fence outcome")),
                            "recording Guardian non-allow",
                        )?;
                    }
                }
            }
            Err(
                PermissionGuardianError::InvalidDecision(_)
                | PermissionGuardianError::TotalDeadline { .. }
                | PermissionGuardianError::Request(_),
            ) => {
                // Keep the exact reviewing claim while the existing human confirmation waiter
                // is pending. It blocks parallel/equivalent reviews without granting an effect.
                self.review_retry_lease = Some(lease);
            }
            _ if fence_outcome.is_some() => {
                require_owned_permission_fence_transition(
                    lease.mark_denied(fence_outcome.expect("guarded by is_some")),
                    "recording Guardian non-allow",
                )?;
            }
            _ => {
                require_owned_permission_fence_transition(
                    lease.release(),
                    "releasing cancelled or failed Guardian review",
                )?;
            }
        }
        result
    }

    fn take_retry_lease(&mut self) -> Option<crate::storage::PermissionReviewLease> {
        self.review_retry_lease.take()
    }
}

fn permission_guardian_total_deadline(
    provider_deadlines: crate::config::ProviderDeadlines,
) -> Duration {
    Duration::from_millis(provider_deadlines.request_timeout_ms)
}

fn permission_guardian_provider_error(
    error: crate::error::LlmError,
) -> crate::tool::permission_guardian::PermissionGuardianError {
    use crate::llm::ProviderFailureKind;
    use crate::tool::permission_guardian::PermissionGuardianError;
    match error.provider_failure().map(|failure| failure.kind) {
        Some(ProviderFailureKind::Cancelled) => PermissionGuardianError::Cancelled,
        Some(ProviderFailureKind::EventProjection) => {
            PermissionGuardianError::UnfencedAdmission(error.public_message())
        }
        _ if matches!(error, crate::error::LlmError::Io(_)) => {
            PermissionGuardianError::UnfencedAdmission(error.public_message())
        }
        _ => PermissionGuardianError::Request(error.public_message()),
    }
}

async fn permission_guardian_review_with_deadline<F>(
    cancel: CancellationToken,
    deadline: Duration,
    review: F,
) -> Result<
    crate::tool::permission_guardian::PermissionGuardianAssessment,
    crate::tool::permission_guardian::PermissionGuardianError,
>
where
    F: std::future::Future<
            Output = Result<
                crate::tool::permission_guardian::PermissionGuardianAssessment,
                crate::tool::permission_guardian::PermissionGuardianError,
            >,
        >,
{
    use crate::tool::permission_guardian::PermissionGuardianError;
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(PermissionGuardianError::Cancelled),
        result = tokio::time::timeout(deadline, review) => match result {
            Ok(result) => result,
            Err(_) => Err(PermissionGuardianError::TotalDeadline {
                milliseconds: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            }),
        },
    }
}

fn require_owned_permission_fence_transition(
    transition: Result<crate::storage::PermissionReviewTransition, crate::error::StorageError>,
    operation: &str,
) -> Result<(), crate::tool::permission_guardian::PermissionGuardianError> {
    use crate::storage::PermissionReviewTransition;
    use crate::tool::permission_guardian::PermissionGuardianError;
    match transition {
        Ok(PermissionReviewTransition::Applied) => Ok(()),
        Ok(PermissionReviewTransition::NotOwned) => {
            Err(PermissionGuardianError::UnfencedAdmission(format!(
                "permission retry fence lost durable ownership while {operation}"
            )))
        }
        Err(error) => Err(PermissionGuardianError::UnfencedAdmission(format!(
            "could not prove durable permission retry-fence ownership while {operation}: {error}"
        ))),
    }
}

fn permission_retry_fence_outcome(
    result: &Result<
        crate::tool::permission_guardian::PermissionGuardianAssessment,
        crate::tool::permission_guardian::PermissionGuardianError,
    >,
) -> Option<crate::storage::PermissionRetryFenceOutcome> {
    use crate::storage::PermissionRetryFenceOutcome;
    use crate::tool::permission_guardian::{PermissionGuardianDecision, PermissionGuardianError};
    match result {
        Ok(assessment) => match assessment.decision() {
            PermissionGuardianDecision::Allow => None,
            PermissionGuardianDecision::Deny | PermissionGuardianDecision::AskUser => {
                Some(PermissionRetryFenceOutcome::GuardianDenied)
            }
        },
        Err(PermissionGuardianError::InvalidDecision(_)) => {
            Some(PermissionRetryFenceOutcome::InvalidDecision)
        }
        Err(PermissionGuardianError::TotalDeadline { .. }) => {
            Some(PermissionRetryFenceOutcome::DeadlineExceeded)
        }
        Err(PermissionGuardianError::Request(_)) => {
            Some(PermissionRetryFenceOutcome::GuardianError)
        }
        Err(
            PermissionGuardianError::Cancelled
            | PermissionGuardianError::RetryFenced(_)
            | PermissionGuardianError::UnfencedAdmission(_),
        ) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardianIsolationTransport {
    Responses,
    ChatCompletions,
}

fn resolved_guardian_isolation_transport(
    target: &crate::config::ProviderTarget,
) -> Result<GuardianIsolationTransport, crate::tool::permission_guardian::PermissionGuardianError> {
    // Admission is owned by the immutable ProviderTarget captured for the turn. Matching on the
    // canonical wire mode (rather than a profile label) admits every verified current profile and
    // makes any future wire mode an explicit compile-time decision instead of a silent allow.
    match target.api_mode() {
        crate::config::ProviderApiMode::Responses => Ok(GuardianIsolationTransport::Responses),
        crate::config::ProviderApiMode::ChatCompletions => {
            Ok(GuardianIsolationTransport::ChatCompletions)
        }
    }
}

async fn account_permission_guardian_goal_usage(
    store: &StoreBundle,
    session_id: crate::session::SessionId,
    goal_id: Option<&str>,
    usage: Option<&TokenUsage>,
) -> Result<(), crate::tool::permission_guardian::PermissionGuardianError> {
    let Some(goal_id) = goal_id else {
        return Ok(());
    };
    store
        .session_repo()
        .account_thread_goal_usage_for_goal(session_id, goal_token_delta(usage), Some(goal_id))
        .await
        .map_err(|error| {
            crate::tool::permission_guardian::PermissionGuardianError::UnfencedAdmission(
                error.to_string(),
            )
        })?;
    Ok(())
}

struct PermissionAuthorityReadState {
    cancelled: AtomicBool,
    interrupt: Mutex<Option<rusqlite::InterruptHandle>>,
}

impl PermissionAuthorityReadState {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            interrupt: Mutex::new(None),
        }
    }

    fn cancel_and_interrupt(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let interrupt = match self.interrupt.lock() {
            Ok(interrupt) => interrupt,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(interrupt) = interrupt.as_ref() {
            interrupt.interrupt();
        }
    }

    fn clear_interrupt(&self) {
        let mut interrupt = match self.interrupt.lock() {
            Ok(interrupt) => interrupt,
            Err(poisoned) => poisoned.into_inner(),
        };
        interrupt.take();
    }

    fn register_interrupt(
        self: &Arc<Self>,
        interrupt: rusqlite::InterruptHandle,
    ) -> Result<PermissionAuthorityWorkerInterrupt, crate::error::StorageError> {
        let query_guard = PermissionAuthorityWorkerInterrupt {
            state: Arc::clone(self),
        };
        {
            let mut installed = self.interrupt.lock().map_err(|_| {
                crate::error::StorageError::Message(
                    "canonical user authority interrupt lock is poisoned".to_string(),
                )
            })?;
            *installed = Some(interrupt);
        }
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(crate::error::StorageError::Message(
                "canonical user authority read was cancelled".to_string(),
            ));
        }
        Ok(query_guard)
    }
}

struct PermissionAuthorityReadCancellation {
    state: Arc<PermissionAuthorityReadState>,
    armed: bool,
}

impl PermissionAuthorityReadCancellation {
    fn new(state: Arc<PermissionAuthorityReadState>) -> Self {
        Self { state, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.state.clear_interrupt();
    }
}

impl Drop for PermissionAuthorityReadCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.state.cancel_and_interrupt();
        }
    }
}

struct PermissionAuthorityWorkerInterrupt {
    state: Arc<PermissionAuthorityReadState>,
}

impl Drop for PermissionAuthorityWorkerInterrupt {
    fn drop(&mut self) {
        self.state.clear_interrupt();
    }
}

struct PermissionGuardianAuthoritySnapshot {
    root_session_id: crate::session::SessionId,
    generation: HistoryItemId,
}

async fn permission_guardian_authority_snapshot(
    store: &StoreBundle,
    request: &AgentRunRequest,
) -> Result<PermissionGuardianAuthoritySnapshot, String> {
    let authority_session_id = permission_guardian_authority_session_id(
        request.session.session.id,
        request.agent_context.as_ref(),
    );
    let store = store.clone();
    let state = Arc::new(PermissionAuthorityReadState::new());
    let mut cancellation = PermissionAuthorityReadCancellation::new(Arc::clone(&state));
    let worker_state = Arc::clone(&state);
    let generation = tokio::task::spawn_blocking(move || {
        if worker_state.cancelled.load(Ordering::SeqCst) {
            return Err("durable canonical user authority read was cancelled".to_string());
        }
        let progress_state = Arc::clone(&worker_state);
        store
            .permission_guardian_authority_store()
            .latest_user_authority_id_for_session_with_interrupt(
                authority_session_id,
                |interrupt| worker_state.register_interrupt(interrupt),
                move || progress_state.cancelled.load(Ordering::SeqCst),
            )
            .map_err(|error| {
                format!(
                    "durable canonical user authority for root session {authority_session_id} could not be read: {error}"
                )
            })
    })
    .await
    .map_err(|error| format!("canonical user authority worker failed: {error}"))?;
    cancellation.disarm();
    let generation = generation?.ok_or_else(|| {
        format!("no canonical UserTurn or SteerTurn exists in root session {authority_session_id}")
    })?;
    Ok(PermissionGuardianAuthoritySnapshot {
        root_session_id: authority_session_id,
        generation,
    })
}

pub(crate) fn permission_guardian_authority_session_id(
    current_session_id: crate::session::SessionId,
    agent_context: Option<&crate::app::AgentRunContext>,
) -> crate::session::SessionId {
    agent_context
        .map(crate::app::AgentRunContext::root_session_id)
        .unwrap_or(current_session_id)
}

#[derive(Default)]
struct ResponseCollector {
    text: String,
    tool_calls: Vec<ModelToolCall>,
    tool_call_order: Vec<String>,
    tool_call_args: HashMap<String, String>,
    tool_call_names: HashMap<String, String>,
    provider_phases: Vec<crate::llm::ProviderPhaseEvent>,
}

impl LlmEventSink for ResponseCollector {
    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.text.push_str(&delta);
            }
            LlmEvent::ReasoningSummaryDelta(_) => {}
            LlmEvent::ToolCallStart { call_id, tool_name } => {
                if !self.tool_call_order.iter().any(|seen| seen == &call_id) {
                    self.tool_call_order.push(call_id.clone());
                }
                self.tool_call_names.insert(call_id.clone(), tool_name);
                self.tool_call_args.entry(call_id).or_default();
            }
            LlmEvent::ToolCallArgsDelta { call_id, delta } => {
                self.tool_call_args
                    .entry(call_id)
                    .or_default()
                    .push_str(&delta);
            }
            LlmEvent::Finished { .. } => {}
        }
        self.rebuild_tool_calls();
        Ok(())
    }

    fn provider_phase(
        &mut self,
        event: crate::llm::ProviderPhaseEvent,
    ) -> Result<(), crate::error::LlmError> {
        self.provider_phases.push(event);
        Ok(())
    }
}

struct StreamingResponseCollector<'a> {
    inner: ResponseCollector,
    response_id: ModelResponseId,
    sink: &'a mut dyn RunEventSink,
    late_diagnostics: Option<(SessionId, usize)>,
}

struct CompactionResponseCollector<'a> {
    inner: ResponseCollector,
    response_id: ModelResponseId,
    sink: &'a mut dyn RunEventSink,
    late_diagnostics: Option<(SessionId, usize)>,
}

impl<'a> CompactionResponseCollector<'a> {
    fn new(response_id: ModelResponseId, sink: &'a mut dyn RunEventSink) -> Self {
        Self {
            inner: ResponseCollector::default(),
            response_id,
            sink,
            late_diagnostics: None,
        }
    }

    fn into_inner(self) -> ResponseCollector {
        self.inner
    }
}

impl LlmEventSink for CompactionResponseCollector<'_> {
    fn request_prepared(
        &mut self,
        request: &ChatRequest,
        public_endpoint: &str,
    ) -> Result<(), crate::error::LlmError> {
        emit_late_request_diagnostics(
            self.sink,
            &mut self.late_diagnostics,
            request,
            public_endpoint,
        )
    }

    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        self.inner.push(event)
    }

    fn provider_phase(
        &mut self,
        event: crate::llm::ProviderPhaseEvent,
    ) -> Result<(), crate::error::LlmError> {
        self.sink
            .emit_runtime_only(RunEvent::ProviderPhase {
                response_id: self.response_id,
                event: event.clone(),
            })
            .map_err(|error| crate::error::LlmError::Message(error.to_string()))?;
        self.inner.provider_phase(event)
    }
}

impl<'a> StreamingResponseCollector<'a> {
    fn new(response_id: ModelResponseId, sink: &'a mut dyn RunEventSink) -> Self {
        Self {
            inner: ResponseCollector::default(),
            response_id,
            sink,
            late_diagnostics: None,
        }
    }

    fn into_inner(self) -> ResponseCollector {
        self.inner
    }
}

impl LlmEventSink for StreamingResponseCollector<'_> {
    fn request_prepared(
        &mut self,
        request: &ChatRequest,
        public_endpoint: &str,
    ) -> Result<(), crate::error::LlmError> {
        emit_late_request_diagnostics(
            self.sink,
            &mut self.late_diagnostics,
            request,
            public_endpoint,
        )
    }

    fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
        match &event {
            LlmEvent::TextDelta(delta) => self
                .sink
                .emit_runtime_only(RunEvent::TextDelta {
                    response_id: self.response_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            LlmEvent::ReasoningSummaryDelta(delta) => self
                .sink
                .emit_runtime_only(RunEvent::ReasoningSummaryDelta {
                    response_id: self.response_id,
                    delta: delta.clone(),
                })
                .map_err(|error| crate::error::LlmError::Message(error.to_string()))?,
            _ => {}
        }
        self.inner.push(event)
    }

    fn provider_phase(
        &mut self,
        event: crate::llm::ProviderPhaseEvent,
    ) -> Result<(), crate::error::LlmError> {
        self.sink
            .emit_runtime_only(RunEvent::ProviderPhase {
                response_id: self.response_id,
                event: event.clone(),
            })
            .map_err(|error| crate::error::LlmError::Message(error.to_string()))?;
        self.inner.provider_phase(event)
    }
}

impl ResponseCollector {
    fn rebuild_tool_calls(&mut self) {
        let calls = self
            .tool_call_order
            .iter()
            .filter_map(|call_id| {
                let tool_name = self.tool_call_names.get(call_id)?;
                Some(ModelToolCall {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments_json: self
                        .tool_call_args
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| "{}".to_string()),
                })
            })
            .collect::<Vec<_>>();
        self.tool_calls = calls;
    }
}

fn run_summary_from_terminal(
    request: &AgentRunRequest,
    terminal: DurableTurnTerminal,
) -> RunSummary {
    RunSummary::from_terminal(request.session.session.id, request.turn_id(), terminal)
}

fn run_metrics(
    hub_route: Option<&crate::hub::HubTurnRoute>,
    request: &AgentRunRequest,
    started_at: Instant,
    model_request_count: usize,
    token_usage: Option<TokenUsage>,
    cumulative_token_usage: &crate::session::CumulativeTokenUsage,
    tool_calls_by_name: &BTreeMap<String, usize>,
    failed_tool_calls_by_name: &BTreeMap<String, usize>,
) -> RunMetrics {
    RunMetrics {
        model_request_count,
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        token_usage,
        cumulative_token_usage: Some(cumulative_token_usage.clone()),
        tool_calls_by_name: tool_calls_by_name.clone(),
        failed_tool_calls_by_name: failed_tool_calls_by_name.clone(),
        config: Some(RunConfigSnapshot {
            model: hub_route.map_or_else(
                || request.model_name().to_string(),
                crate::hub::HubTurnRoute::metrics_model,
            ),
            base_url: request
                .turn
                .provider_target()
                .sanitized_endpoint()
                .to_string(),
            access_mode: request
                .turn
                .resolved_config()
                .runtime_config()
                .permissions
                .access_mode,
        }),
    }
}

fn emit_late_request_diagnostics(
    sink: &mut dyn RunEventSink,
    pending: &mut Option<(SessionId, usize)>,
    request: &ChatRequest,
    public_endpoint: &str,
) -> Result<(), crate::error::LlmError> {
    if let Some((session_id, overflow_margin_tokens)) = pending.take() {
        let mut diagnostics = request_diagnostics(request, overflow_margin_tokens, None);
        diagnostics.base_url = public_endpoint.to_string();
        sink.emit(RunEvent::ModelRequestPrepared {
            session_id,
            diagnostics,
        })
        .map_err(|error| crate::error::LlmError::Message(error.to_string()))?;
    }
    Ok(())
}

fn request_diagnostics(
    request: &ChatRequest,
    overflow_margin_tokens: usize,
    context_window: Option<ContextWindowTokenStatus>,
) -> RequestDiagnosticsPart {
    let context_window = context_window
        .unwrap_or_else(|| ContextWindowTokenStatus::for_request(request, overflow_margin_tokens));
    let request_timeout_ms = request.provider_target().deadlines().request_timeout_ms;
    RequestDiagnosticsPart {
        provider: "openai_compat".to_string(),
        model_name: request.model.name.clone(),
        base_url: request.provider_target().sanitized_endpoint().to_string(),
        request_timeout_ms,
        stream_idle_timeout_ms: request_timeout_ms,
        configured_max_output_tokens: None,
        effective_max_output_tokens: None,
        output_budget_reason: None,
        supports_tools: Some(request.model.capabilities.supports_tools),
        // Historical diagnostics may contain this field, but new requests do
        // not project a moyAI-owned reasoning setting or capability decision.
        supports_reasoning: None,
        supports_images: Some(request.model.capabilities.supports_images),
        system_prompt_chars: request.system_prompt.chars().count(),
        tool_count: request.tools.len(),
        tool_choice: request
            .tool_choice
            .as_ref()
            .map(|choice| choice.diagnostic_label())
            .or_else(|| (!request.tools.is_empty()).then(|| "auto".to_string())),
        parallel_tool_calls: crate::llm::tool_surface_scoped_parallel_tool_calls_projection(
            request.tools.len(),
            request.parallel_tool_calls,
        ),
        provider_message_count: request.messages.len(),
        image_count: request.messages.iter().map(message_image_count).sum(),
        image_bytes: request.messages.iter().map(message_image_bytes).sum(),
        tool_names: request.tools.iter().map(|tool| tool.name.clone()).collect(),
        tool_schemas: request
            .tools
            .iter()
            .map(|tool| RequestToolSchemaDiagnostic {
                name: tool.name.clone(),
                description_chars: tool.description.chars().count(),
                input_schema: tool.input_schema.clone(),
            })
            .collect(),
        wire: crate::llm::request_diagnostics::http_request_wire_diagnostic(request).ok(),
        context_window: Some(context_window),
        messages: request.messages.iter().map(message_diagnostic).collect(),
    }
}

fn message_diagnostic(message: &ModelMessage) -> RequestMessageDiagnostic {
    match message {
        ModelMessage::System { content } => RequestMessageDiagnostic {
            role: "system".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::Developer { content } => RequestMessageDiagnostic {
            role: "developer".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::Agent { content } => RequestMessageDiagnostic {
            role: "agent".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::User { content } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::UserParts { parts } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        ModelContentPart::Text { text } => Some(text.chars().count()),
                        ModelContentPart::Image { .. } => None,
                    })
                    .sum(),
            ),
            content_markers: Vec::new(),
            image_count: message_image_count(message),
            image_bytes: message_image_bytes(message),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::Assistant { content } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
            ..
        } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: content.as_ref().map(|value| value.chars().count()),
            content_markers: content.as_deref().map(content_markers).unwrap_or_default(),
            image_count: 0,
            image_bytes: 0,
            tool_calls: tool_calls
                .iter()
                .map(|call| RequestToolCallDiagnostic {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    arguments_chars: call.arguments_json.chars().count(),
                })
                .collect(),
            tool_call_id: None,
        },
        ModelMessage::Tool {
            call_id, result, ..
        } => RequestMessageDiagnostic {
            role: "tool".to_string(),
            content_chars: Some(result.chars().count()),
            content_markers: content_markers(result),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.clone()),
        },
    }
}

fn message_image_count(message: &ModelMessage) -> usize {
    match message {
        ModelMessage::UserParts { parts } => parts
            .iter()
            .filter(|part| matches!(part, ModelContentPart::Image { .. }))
            .count(),
        _ => 0,
    }
}

fn message_image_bytes(message: &ModelMessage) -> u64 {
    match message {
        ModelMessage::UserParts { parts } => parts
            .iter()
            .filter_map(|part| match part {
                ModelContentPart::Image { data_base64, .. } => {
                    base64::engine::general_purpose::STANDARD
                        .decode(data_base64)
                        .ok()
                        .map(|bytes| bytes.len() as u64)
                }
                ModelContentPart::Text { .. } => None,
            })
            .sum(),
        _ => 0,
    }
}

fn content_markers(content: &str) -> Vec<String> {
    let mut markers = Vec::new();
    for marker in ["<world_state>", "<instructions>", "<environment_context>"] {
        if content.contains(marker) {
            markers.push(marker.to_string());
        }
    }
    markers
}

#[cfg(test)]
fn messages_from_history(history_items: &[HistoryItem]) -> Vec<ModelMessage> {
    context_manager::ContextManager::rehydrate(history_items.to_vec()).model_messages(true)
}

#[cfg(test)]
fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_tool_arguments(arguments_json: &str) -> Result<Value, AgentError> {
    serde_json::from_str(arguments_json).map_err(|error| {
        let digest = Sha256::digest(arguments_json.as_bytes());
        let preview = bounded_utf8_prefix(arguments_json, 512);
        let preview = serde_json::to_string(preview).unwrap_or_else(|_| "\"<unavailable>\"".into());
        AgentError::Message(format!(
            "invalid tool arguments JSON at line {}, column {}; input_bytes={}; input_sha256={digest:x}; preview={preview}",
            error.line(),
            error.column(),
            arguments_json.len(),
        ))
    })
}

fn bounded_utf8_prefix(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

fn validate_shallow_schema(
    tool_name: &str,
    arguments: Value,
    schemas: &[ToolSchema],
) -> Result<Value, AgentError> {
    let Some(schema) = schemas.iter().find(|schema| schema.name == tool_name) else {
        return Ok(arguments);
    };
    let mut errors = Vec::new();
    validate_json_schema_value(&arguments, &schema.input_schema, "$", &mut errors);
    if errors.is_empty() {
        Ok(arguments)
    } else {
        Err(AgentError::Message(format!(
            "tool `{tool_name}` arguments do not match schema: {}",
            errors.join("; ")
        )))
    }
}

fn validate_json_schema_value(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(expected) = schema.get("type") {
        validate_json_type(value, expected, path, errors);
    }
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        errors.push(format!("{path} is not one of the allowed enum values"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    errors.push(format!("{path}.{key} is required"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, property_schema) in properties {
                if let Some(property_value) = object.get(key) {
                    validate_json_schema_value(
                        property_value,
                        property_schema,
                        &format!("{path}.{key}"),
                        errors,
                    );
                }
            }
        }
    }
    if let Some(items_schema) = schema.get("items")
        && let Some(items) = value.as_array()
    {
        for (index, item) in items.iter().enumerate() {
            validate_json_schema_value(item, items_schema, &format!("{path}[{index}]"), errors);
        }
    }
}

fn validate_json_type(value: &Value, expected: &Value, path: &str, errors: &mut Vec<String>) {
    let matches = match expected {
        Value::String(kind) => json_type_matches(value, kind),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| json_type_matches(value, kind)),
        _ => true,
    };
    if !matches {
        errors.push(format!("{path} expected type {expected}"));
    }
}

fn json_type_matches(value: &Value, kind: &str) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn failed_tool_metadata(mut metadata: Value) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert("success".to_string(), Value::Bool(false));
    }
    metadata
}

fn tool_cleanup_diagnostic_metadata(mut metadata: Value, diagnostic: &str) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "cancellation_cleanup_error".to_string(),
            Value::String(diagnostic.to_string()),
        );
    }
    metadata
}

fn tool_result_text(result: &ToolResult) -> String {
    if result.output_text.trim().is_empty() {
        result.title.clone()
    } else {
        result.output_text.clone()
    }
}

fn merge_tool_metadata(mut metadata: Value, result: &ToolResult) -> Value {
    if let Some(object) = metadata.as_object_mut() {
        let success = result
            .metadata
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        object.insert("tool_metadata".to_string(), result.metadata.clone());
        object.insert("success".to_string(), Value::Bool(success));
    }
    metadata
}

fn goal_token_delta(usage: Option<&TokenUsage>) -> i64 {
    usage
        .map(|usage| i64::from(usage.total_tokens))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
