use std::fs;

use base64::Engine as _;
use camino::{Utf8Path, Utf8PathBuf};

use crate::agent::{AgentLoop, AgentRunRequest, RuntimeInputView};
use crate::app::session_title::{generate_session_title, is_placeholder_session_title};
use crate::app::{AppCommand, ReviewRequest, RunRequest, SessionListRequest, SessionShowRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::error::{AppRunError, RuntimeError};
use crate::harness::{HarnessRecordingSink, NativeHarnessRecorder};
use crate::llm::{
    ConfigModelCatalog, ModelCatalog, apply_provider_model_info_to_config,
    fetch_provider_model_infos,
};
use crate::protocol::{
    ActiveWorkContractProjection, ModelCapabilities as ProtocolModelCapabilities, OutputContract,
    ProjectionId, ProtocolEventStore, ProtocolRecordingSink, SandboxProfile, ThreadOp, ToolChoice,
    TurnContext, UserInputItem, UserTurn,
};
use crate::runtime::RunEventSink;
use crate::session::{
    DispatchTransformKind, ImagePart, PromptDispatchPart, RunSummary, SessionRepository,
    SessionSelector, SessionStartRequest, SessionStateSnapshot, SessionStatus, TaskRoute,
    Transcript,
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
}

impl RunService {
    pub fn new(
        store: StoreBundle,
        config: crate::config::ResolvedConfig,
        workspace: crate::workspace::Workspace,
        session_service: crate::session::SessionService,
        agent_loop: AgentLoop,
    ) -> Self {
        Self {
            store,
            config,
            workspace,
            session_service,
            agent_loop,
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
            AppCommand::SessionList(request) => self.execute_session_list(request, renderer).await,
            AppCommand::SessionShow(request) => self.execute_session_show(request, renderer).await,
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
        let session = self.store.session_repo().get_session(session_id).await?;
        let runtime_input = RuntimeInputView::from_history_items(&session, history_items);
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
        let mut effective_config = match request.config_override.clone() {
            Some(patch) => apply_config_patch(self.config.clone(), patch),
            None => self.config.clone(),
        };
        if !request.base_url.trim().is_empty() {
            effective_config.model.base_url = request.base_url.clone();
        }
        if !request.model.trim().is_empty() {
            effective_config.model.model = request.model.clone();
        }
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
        let should_generate_session_title = matches!(&selector, SessionSelector::New)
            && request
                .title
                .as_deref()
                .map(is_placeholder_session_title)
                .unwrap_or(false)
            && !request.prompt.trim().is_empty();
        let resuming_interrupted_session = self.session_was_running(&selector).await?;
        hydrate_configured_model_from_provider(&mut effective_config).await?;
        let model = ConfigModelCatalog::new(effective_config.clone()).resolve(None)?;
        let image_parts = load_image_attachments(&request.cwd, &request.image_paths)?;
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
                },
                self.workspace.clone(),
            )
            .await?;
        let prepared = prepare_run_turn(&self.workspace, &request)?;
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
        );
        sink.emit(crate::session::RunEvent::SessionStarted {
            session_id: session_context.session.id,
            title: session_context.session.title.clone(),
        })?;
        if resuming_interrupted_session {
            sink.emit(crate::session::RunEvent::SessionFailed {
                session_id: session_context.session.id,
                message: "Previous run was interrupted.".to_string(),
            })?;
        }

        let user_message_id = if prepared.prompt.trim().is_empty() {
            let runtime_input = self.runtime_input_view(session_context.session.id).await?;
            runtime_input
                .history_items
                .iter()
                .rev()
                .find_map(|item| match &item.payload {
                    crate::protocol::HistoryItemPayload::UserTurn {
                        message_id: Some(message_id),
                        ..
                    } => Some(*message_id),
                    _ => None,
                })
                .ok_or_else(|| {
                    AppRunError::Message(
                        "cannot resume a session without a prompt or prior user message"
                            .to_string(),
                    )
                })?
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
                .store_user_thread_op(
                    &session_context,
                    user_turn,
                    Some(effective_config.model.model.clone()),
                    prepared.initial_state.clone(),
                )
                .await?;
            sink.emit(crate::session::RunEvent::UserTurnStored {
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
                        if self
                            .store
                            .session_repo()
                            .update_session_title(session_context.session.id, &title)
                            .await
                            .is_ok()
                        {
                            sink.emit(crate::session::RunEvent::SessionTitleUpdated {
                                session_id: session_context.session.id,
                                title,
                            })?;
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
                        self.store
                            .session_repo()
                            .set_status(session_id, SessionStatus::Cancelled)
                            .await?;
                        sink.emit(crate::session::RunEvent::SessionInterrupted {
                            session_id,
                            reason: "run cancelled by user".to_string(),
                        })?;
                    } else {
                        self.store
                            .session_repo()
                            .set_status(session_id, SessionStatus::Failed)
                            .await?;
                        sink.emit(crate::session::RunEvent::SessionFailed {
                            session_id,
                            message: error.to_string(),
                        })?;
                    }
                }
                return Err(error.into());
            }
        };
        drop(sink);
        renderer.finish(&summary)?;
        Ok(summary)
    }

    async fn session_was_running(&self, selector: &SessionSelector) -> Result<bool, AppRunError> {
        let repository = self.store.session_repo();
        let session = match selector {
            SessionSelector::New => return Ok(false),
            SessionSelector::ById(id) => repository.get_session(*id).await?,
            SessionSelector::Latest => {
                match repository.latest_session(self.workspace.project_id).await? {
                    Some(session) => session,
                    None => return Ok(false),
                }
            }
        };
        Ok(session.status == SessionStatus::Running)
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
        let transcript = self
            .runtime_input_view(request.session_id)
            .await?
            .into_compatibility_transcript();
        let transcript = if request.show_reasoning {
            transcript
        } else {
            strip_reasoning(transcript)
        };
        renderer.render_session_history_items(&session, &history_items, &transcript)?;
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
) -> Result<(), AppRunError> {
    let configured_model = config.model.model.trim().to_string();
    if configured_model.is_empty() {
        return Err(AppRunError::Message(
            "configured model is empty".to_string(),
        ));
    }

    let models = fetch_provider_model_infos(config, &config.model.base_url).await?;
    let Some(model) = models.iter().find(|model| model.id == configured_model) else {
        let available = models
            .iter()
            .take(12)
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppRunError::Message(format!(
            "configured model `{configured_model}` is not available at `{}`; available models: {available}",
            crate::llm::normalize_provider_base_url(&config.model.base_url),
        )));
    };
    apply_provider_model_info_to_config(&mut config.model, model);
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
            parallel_tool_calls: config.model.parallel_tool_calls,
            context_window: config.model.context_window,
            max_output_tokens: config.model.max_output_tokens,
        },
        route: state.route,
        process_phase: state.process_phase,
        active_contract: ActiveWorkContractProjection {
            route: state.route,
            process_phase: state.process_phase,
            active_work_kind: Some(format!("{:?}", state.route)),
            summary: "Initial user turn context before reducer projection.".to_string(),
            active_targets: state.active_targets.clone(),
            operation_intents: Vec::new(),
            required_next_action: None,
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

fn strip_reasoning(mut transcript: Transcript) -> Transcript {
    for message in &mut transcript.messages {
        message
            .parts
            .retain(|part| !matches!(part.kind, crate::session::PartKind::Reasoning));
    }
    transcript
}
