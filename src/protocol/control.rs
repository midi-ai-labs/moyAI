use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::{
    ModelCapabilities, OperationIntent, ProjectionId, ToolChoice, TurnContext,
    TurnControlEnvelopeId, TurnId,
};
use crate::session::SessionId;
use crate::tool::{ToolName, shell::command_text_encoding_suggested_command};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnControlEnvelope {
    pub id: TurnControlEnvelopeId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub projection_id: ProjectionId,
    pub context: TurnContext,
    pub obligations: ObligationSet,
    pub action_authority: ActionAuthority,
    pub projection_bundle: ProjectionBundle,
    pub dispatch_policy: DispatchPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
}

impl TurnControlEnvelope {
    pub fn new(
        turn_id: TurnId,
        context: TurnContext,
        obligations: ObligationSet,
        action_authority: ActionAuthority,
        projection_bundle: ProjectionBundle,
        dispatch_policy: DispatchPolicy,
        evidence_refs: Vec<EvidenceRef>,
    ) -> Self {
        let projection_id = action_authority.projection_id;
        Self {
            id: TurnControlEnvelopeId::new(),
            session_id: context.session_id,
            turn_id,
            projection_id,
            context,
            obligations,
            action_authority,
            projection_bundle,
            dispatch_policy,
            evidence_refs,
        }
    }

    pub fn validate(&self) -> ControlEnvelopeValidation {
        let mut validation = ControlEnvelopeValidation::default();

        if self.session_id != self.context.session_id {
            validation.push_error(
                ControlEnvelopeIssueCode::SessionMismatch,
                "envelope session_id differs from turn context session_id",
            );
        }

        validate_projection_id(
            &mut validation,
            "action_authority",
            self.projection_id,
            self.action_authority.projection_id,
        );
        validate_projection_id(
            &mut validation,
            "projection_bundle",
            self.projection_id,
            self.projection_bundle.projection_id,
        );
        for surface in self.projection_bundle.surfaces() {
            validate_projection_id(
                &mut validation,
                surface.surface.as_str(),
                self.projection_id,
                surface.projection_id,
            );
        }

        if self.context.active_contract.projection_id != self.projection_id {
            validation.push_error(
                ControlEnvelopeIssueCode::ProjectionIdMismatch,
                "active work contract projection_id differs from control envelope projection_id",
            );
        }

        if !same_tool_set(
            &self.context.allowed_tools,
            &self.context.active_contract.allowed_tools,
        ) {
            validation.push_error(
                ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
                "turn context allowed tools differ from active contract allowed tools",
            );
        }

        if !tool_set_is_subset(
            &self.action_authority.allowed_tools,
            &self.context.allowed_tools,
        ) {
            validation.push_error(
                ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
                "action authority allowed tools must be compiled from the turn context allowed surface",
            );
        }

        for surface in self.projection_bundle.surfaces() {
            if !same_tool_set(&surface.allowed_tools, &self.action_authority.allowed_tools) {
                validation.push_error(
                    ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
                    format!(
                        "{} projection allowed tools differ from action authority",
                        surface.surface.as_str()
                    ),
                );
            }
            if !same_tool_set(
                &surface.forbidden_tools,
                &self.action_authority.forbidden_tools,
            ) {
                validation.push_error(
                    ControlEnvelopeIssueCode::ForbiddenSurfaceMismatch,
                    format!(
                        "{} projection forbidden tools differ from action authority",
                        surface.surface.as_str()
                    ),
                );
            }
        }

        validate_tool_choice(
            &mut validation,
            &self.action_authority.tool_choice,
            &self.action_authority.allowed_tools,
        );

        validate_model_capabilities(
            &mut validation,
            &self.context.model_capabilities,
            !self.context.images.is_empty(),
            &self.action_authority.allowed_tools,
            &self.action_authority.tool_choice,
        );

        if self.obligations.has_open_obligations()
            && matches!(self.dispatch_policy, DispatchPolicy::Complete { .. })
        {
            validation.push_error(
                ControlEnvelopeIssueCode::CompletionWithOpenObligations,
                "completion dispatch policy cannot be selected while obligations remain open",
            );
        }

        if matches!(self.dispatch_policy, DispatchPolicy::Dispatch)
            && self.action_authority.allowed_tools.is_empty()
            && !matches!(self.action_authority.tool_choice, ToolChoice::None)
        {
            validation.push_error(
                ControlEnvelopeIssueCode::DispatchWithoutSurface,
                "provider dispatch requires a non-empty tool surface unless tool_choice is none",
            );
        }

        if matches!(self.dispatch_policy, DispatchPolicy::Dispatch)
            && self.obligations.items.is_empty()
            && !self
                .action_authority
                .required_verification_commands
                .is_empty()
        {
            validation.push_error(
                ControlEnvelopeIssueCode::DispatchWithoutOpenObligations,
                "provider dispatch with verification commands requires a compiled ObligationSet",
            );
        }
        validate_obligation_authority_alignment(
            &mut validation,
            &self.action_authority,
            &self.obligations,
        );

        validation
    }

    pub fn fail_closed_before_dispatch(&self) -> Option<&str> {
        if self.validate().has_errors() {
            return Some("control envelope validation failed");
        }
        match &self.dispatch_policy {
            DispatchPolicy::FailClosed { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationSet {
    #[serde(default)]
    pub items: Vec<TurnObligation>,
}

impl ObligationSet {
    pub fn new(items: Vec<TurnObligation>) -> Self {
        Self { items }
    }

    pub fn empty() -> Self {
        Self { items: Vec::new() }
    }

    pub fn has_open_obligations(&self) -> bool {
        self.items.iter().any(|item| item.status.is_open())
    }

    pub fn open_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status.is_open())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnObligation {
    pub obligation_id: String,
    pub kind: ObligationKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_intents: Vec<OperationIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contract_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
    pub status: ObligationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationKind {
    UserWork,
    Verification,
    Contract,
    Repair,
    Closeout,
    Continuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationStatus {
    Open,
    Satisfied,
    Blocked,
}

impl ObligationStatus {
    fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Blocked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAuthority {
    pub projection_id: ProjectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_action: Option<RequiredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_intents: Vec<OperationIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<ToolName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<ToolName>,
    pub tool_choice: ToolChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredAction {
    pub kind: RequiredActionKind,
    pub tool: ToolName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub projection_text: String,
}

impl RequiredAction {
    fn shell(command: String) -> Self {
        Self {
            kind: RequiredActionKind::ShellCommand,
            tool: ToolName::Shell,
            target: None,
            projection_text: format!("shell:{command}"),
            command: Some(command),
        }
    }

    fn edit(tool: ToolName, target: Utf8PathBuf) -> Self {
        let prefix = match tool {
            ToolName::ApplyPatch => "apply_patch",
            ToolName::Write => "write",
            _ => "edit",
        };
        Self {
            kind: RequiredActionKind::EditTarget,
            tool,
            projection_text: format!("{prefix}:{target}"),
            target: Some(target),
            command: None,
        }
    }

    pub fn projection_label(&self) -> &str {
        &self.projection_text
    }

    pub fn edit_target(&self) -> Option<&Utf8Path> {
        if self.kind == RequiredActionKind::EditTarget {
            self.target.as_deref()
        } else {
            None
        }
    }

    pub fn shell_command(&self) -> Option<&str> {
        if self.kind == RequiredActionKind::ShellCommand && self.tool == ToolName::Shell {
            self.command.as_deref()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredActionKind {
    ShellCommand,
    EditTarget,
}

impl ActionAuthority {
    pub fn from_obligations(
        context: &TurnContext,
        obligations: &ObligationSet,
        tool_choice: ToolChoice,
    ) -> Self {
        let open_obligations = obligations
            .items
            .iter()
            .filter(|item| item.status.is_open())
            .collect::<Vec<_>>();

        let mut required_verification_commands = open_obligations
            .iter()
            .flat_map(|item| item.verification_commands.iter().cloned())
            .collect::<Vec<_>>();
        required_verification_commands.extend(
            context
                .active_contract
                .required_verification_commands
                .iter()
                .cloned(),
        );
        required_verification_commands.sort();
        required_verification_commands.dedup();
        let mut operation_intents = open_obligations
            .iter()
            .flat_map(|item| item.operation_intents.iter().copied())
            .collect::<Vec<_>>();
        operation_intents.sort_by_key(|intent| intent.as_str());
        operation_intents.dedup();

        let verification_command_only = !required_verification_commands.is_empty()
            && operation_intents.is_empty()
            && open_obligations
                .iter()
                .any(|item| item.kind == ObligationKind::Verification)
            && open_obligations
                .iter()
                .all(|item| item.operation_intents.is_empty());
        let mut allowed_tools = context.allowed_tools.clone();
        allowed_tools.sort_by_key(|tool| tool.to_string());
        allowed_tools.dedup();
        if verification_command_only && allowed_tools.contains(&ToolName::Shell) {
            allowed_tools.retain(|tool| *tool == ToolName::Shell);
        }
        let singleton_authoring_target = if operation_intents
            .contains(&OperationIntent::ContentChangingAuthoringRequired)
            && (allowed_tools.contains(&ToolName::ApplyPatch)
                || allowed_tools.contains(&ToolName::Write))
        {
            let raw_targets = open_obligations
                .iter()
                .flat_map(|item| item.targets.iter().cloned())
                .collect::<Vec<_>>();
            let mut targets = canonicalize_workspace_targets(&raw_targets, &context.workspace_root);
            targets.sort();
            targets.dedup();
            if targets.len() == 1 {
                Some(targets.remove(0))
            } else {
                None
            }
        } else {
            None
        };
        let explicit_required_action = open_obligations
            .iter()
            .flat_map(|item| item.required_actions.iter())
            .filter_map(|action| parse_obligation_required_action(action, &context.workspace_root))
            .try_fold(
                None,
                |selected: Option<RequiredAction>, action| match selected {
                    Some(existing) if existing != action => Err(()),
                    Some(existing) => Ok(Some(existing)),
                    None => Ok(Some(action)),
                },
            )
            .ok()
            .flatten()
            .filter(|action| {
                allowed_tools.contains(&action.tool)
                    || (action.tool == ToolName::Write
                        && allowed_tools.contains(&ToolName::ApplyPatch))
            });
        let required_action =
            if verification_command_only && required_verification_commands.len() == 1 {
                let executable = executable_verification_command_for_shell(
                    &required_verification_commands[0],
                    context.shell_family,
                );
                Some(RequiredAction::shell(executable))
            } else if explicit_required_action.is_some() {
                explicit_required_action
            } else {
                singleton_authoring_target.map(|target| {
                    if matches!(&tool_choice, ToolChoice::Named(tool) if *tool == ToolName::Write)
                        && allowed_tools.contains(&ToolName::Write)
                    {
                        RequiredAction::edit(ToolName::Write, target)
                    } else if allowed_tools.contains(&ToolName::ApplyPatch) {
                        RequiredAction::edit(ToolName::ApplyPatch, target)
                    } else {
                        RequiredAction::edit(ToolName::Write, target)
                    }
                })
            };
        let tool_choice =
            compile_tool_choice(required_action.as_ref(), &allowed_tools, tool_choice);

        Self {
            projection_id: context.active_contract.projection_id,
            required_action,
            required_verification_commands,
            operation_intents,
            allowed_tools,
            forbidden_tools: context.active_contract.forbidden_tools.clone(),
            tool_choice,
        }
    }

    pub fn required_action_tool(&self) -> Option<ToolName> {
        self.required_action
            .as_ref()
            .map(|action| action.tool.clone())
    }

    pub fn required_action_is_allowed(&self) -> bool {
        self.required_action_tool().is_none_or(|tool| {
            self.allowed_tools.contains(&tool)
                || (tool == ToolName::Write && self.allowed_tools.contains(&ToolName::ApplyPatch))
        })
    }
}

fn parse_obligation_required_action(
    raw: &str,
    workspace_root: &Utf8PathBuf,
) -> Option<RequiredAction> {
    let (tool, payload) = raw.split_once(':')?;
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }
    match tool.trim() {
        "apply_patch" => {
            canonicalize_workspace_targets(&[Utf8PathBuf::from(payload)], workspace_root)
                .into_iter()
                .next()
                .map(|target| RequiredAction::edit(ToolName::ApplyPatch, target))
        }
        "write" => canonicalize_workspace_targets(&[Utf8PathBuf::from(payload)], workspace_root)
            .into_iter()
            .next()
            .map(|target| RequiredAction::edit(ToolName::Write, target)),
        "shell" => Some(RequiredAction::shell(payload.to_string())),
        _ => None,
    }
}

fn executable_verification_command_for_shell(
    command: &str,
    shell_family: crate::config::ShellFamily,
) -> String {
    command_text_encoding_suggested_command(command, shell_family)
        .unwrap_or_else(|| command.to_string())
}

pub(crate) fn canonicalize_workspace_targets(
    targets: &[Utf8PathBuf],
    workspace_root: &Utf8PathBuf,
) -> Vec<Utf8PathBuf> {
    let mut by_identity = BTreeMap::<String, Utf8PathBuf>::new();
    for target in targets {
        let Some((identity, display_target)) =
            canonical_workspace_target_identity(target, workspace_root)
        else {
            continue;
        };
        by_identity
            .entry(identity)
            .and_modify(|existing| {
                if prefer_workspace_target_display(&display_target, existing) {
                    *existing = display_target.clone();
                }
            })
            .or_insert(display_target);
    }
    by_identity.into_values().collect()
}

fn canonical_workspace_target_identity(
    target: &Utf8PathBuf,
    workspace_root: &Utf8PathBuf,
) -> Option<(String, Utf8PathBuf)> {
    let target_text = target.as_str();
    let relative = crate::workspace::project::workspace_relative_key_for_match(
        target_text,
        workspace_root.as_str(),
    );
    let identity = relative
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::workspace::project::path_key_for_workspace_match(target_text));
    if identity.trim().is_empty() {
        return None;
    }
    let display_target = relative
        .filter(|value| !value.trim().is_empty())
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| target.clone());
    Some((identity, display_target))
}

fn prefer_workspace_target_display(candidate: &Utf8PathBuf, existing: &Utf8PathBuf) -> bool {
    if existing.is_absolute() && !candidate.is_absolute() {
        return true;
    }
    if candidate.is_absolute() && !existing.is_absolute() {
        return false;
    }
    candidate.as_str().len() < existing.as_str().len()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionBundle {
    pub projection_id: ProjectionId,
    pub prompt: ProjectionSurface,
    pub tool_result_feedback: ProjectionSurface,
    pub request_diagnostics: ProjectionSurface,
    pub handoff: ProjectionSurface,
    pub preflight: ProjectionSurface,
}

impl ProjectionBundle {
    pub fn from_authority_and_obligations(
        authority: &ActionAuthority,
        obligations: &ObligationSet,
    ) -> Self {
        Self {
            projection_id: authority.projection_id,
            prompt: ProjectionSurface::from_authority_and_obligations(
                ProjectionSurfaceKind::Prompt,
                authority,
                obligations,
            ),
            tool_result_feedback: ProjectionSurface::from_authority_and_obligations(
                ProjectionSurfaceKind::ToolResultFeedback,
                authority,
                obligations,
            ),
            request_diagnostics: ProjectionSurface::from_authority_and_obligations(
                ProjectionSurfaceKind::RequestDiagnostics,
                authority,
                obligations,
            ),
            handoff: ProjectionSurface::from_authority_and_obligations(
                ProjectionSurfaceKind::Handoff,
                authority,
                obligations,
            ),
            preflight: ProjectionSurface::from_authority_and_obligations(
                ProjectionSurfaceKind::Preflight,
                authority,
                obligations,
            ),
        }
    }

    pub fn surfaces(&self) -> [&ProjectionSurface; 5] {
        [
            &self.prompt,
            &self.tool_result_feedback,
            &self.request_diagnostics,
            &self.handoff,
            &self.preflight,
        ]
    }

    pub fn rendered_surfaces(&self) -> Vec<RenderedProjectionSurface> {
        self.surfaces()
            .into_iter()
            .map(ProjectionSurface::render_control_projection)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSurface {
    pub surface: ProjectionSurfaceKind,
    pub projection_id: ProjectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_action: Option<RequiredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<ToolName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<ToolName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_intents: Vec<OperationIntent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligation_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contract_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
}

impl ProjectionSurface {
    pub fn from_authority_and_obligations(
        surface: ProjectionSurfaceKind,
        authority: &ActionAuthority,
        obligations: &ObligationSet,
    ) -> Self {
        let open_obligations = obligations
            .items
            .iter()
            .filter(|item| item.status.is_open())
            .collect::<Vec<_>>();
        let mut obligation_ids = open_obligations
            .iter()
            .map(|item| item.obligation_id.clone())
            .collect::<Vec<_>>();
        obligation_ids.sort();
        obligation_ids.dedup();
        let mut contract_refs = open_obligations
            .iter()
            .flat_map(|item| item.contract_refs.iter().cloned())
            .collect::<Vec<_>>();
        contract_refs.sort();
        contract_refs.dedup();
        let mut evidence_refs = open_obligations
            .iter()
            .flat_map(|item| item.evidence_refs.iter().cloned())
            .collect::<Vec<_>>();
        evidence_refs.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.reference.cmp(&right.reference))
        });
        evidence_refs.dedup_by(|left, right| {
            left.source == right.source && left.reference == right.reference
        });

        Self {
            surface,
            projection_id: authority.projection_id,
            required_action: authority.required_action.clone(),
            allowed_tools: authority.allowed_tools.clone(),
            forbidden_tools: authority.forbidden_tools.clone(),
            operation_intents: authority.operation_intents.clone(),
            obligation_ids,
            contract_refs,
            evidence_refs,
        }
    }

    pub fn render_control_projection(&self) -> RenderedProjectionSurface {
        self.render_with_body(control_projection_body(self.surface))
    }

    pub fn render_prompt_block(&self) -> String {
        self.render_control_projection().text
    }

    pub fn render_handoff_block(&self, handoff_details: Option<&str>) -> String {
        let mut body = control_projection_body(self.surface).to_string();
        if let Some(details) = handoff_details
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body.push('\n');
            body.push_str(details);
        }
        self.render_with_body(body).text
    }

    pub fn render_tool_result_feedback(
        &self,
        requested_tool_name: &str,
        effective_tool_name: &str,
        recovery_hint: Option<&str>,
    ) -> RenderedProjectionSurface {
        let allowed = sorted_tool_labels(&self.allowed_tools);
        let mut body = if allowed.is_empty() {
            "No tools are available in the current completion-only state. Respond with assistant text only.".to_string()
        } else {
            format!(
                "The `{requested_tool_name}` tool is not available in the current run state. Allowed tools for this turn: {}. Use only those tools until the current recovery or completion gate clears.",
                allowed.join(", ")
            )
        };
        if requested_tool_name != effective_tool_name {
            body.push_str(&format!(
                " The runtime resolved the request to `{effective_tool_name}` before applying this surface."
            ));
        }
        if let Some(hint) = recovery_hint
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body.push(' ');
            body.push_str(hint);
        }
        self.render_with_body(body)
    }

    pub fn render_model_action_rejection_feedback(
        &self,
        requested_tool_name: &str,
        effective_tool_name: &str,
        semantic_class: &str,
        recovery_hint: Option<&str>,
    ) -> RenderedProjectionSurface {
        if semantic_class != "malformed_tool_arguments"
            && semantic_class != "schema_outside_tool_proposal"
        {
            return self.render_tool_result_feedback(
                requested_tool_name,
                effective_tool_name,
                recovery_hint,
            );
        }
        let allowed = sorted_tool_labels(&self.allowed_tools);
        let mut body = match semantic_class {
            "malformed_tool_arguments" => format!(
                "The provider emitted malformed arguments for `{requested_tool_name}`. The tool may be available, but this submitted payload was not executable JSON for the current tool schema. Allowed tools for this turn: {}. Submit a valid tool-call argument object before any final assistant message.",
                if allowed.is_empty() {
                    "none".to_string()
                } else {
                    allowed.join(", ")
                }
            ),
            "schema_outside_tool_proposal" => format!(
                "The provider emitted a payload outside the configured schema for `{requested_tool_name}`. Allowed tools for this turn: {}. Submit an object matching the selected tool schema before any final assistant message.",
                if allowed.is_empty() {
                    "none".to_string()
                } else {
                    allowed.join(", ")
                }
            ),
            _ => unreachable!(),
        };
        if requested_tool_name != effective_tool_name {
            body.push_str(&format!(
                " The runtime resolved the request to `{effective_tool_name}` before applying this surface."
            ));
        }
        if let Some(hint) = recovery_hint
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body.push(' ');
            body.push_str(hint);
        }
        self.render_with_body(body)
    }

    fn render_with_body(&self, body: impl Into<String>) -> RenderedProjectionSurface {
        let allowed_tools = sorted_tool_labels(&self.allowed_tools);
        let forbidden_tools = sorted_tool_labels(&self.forbidden_tools);
        let mut lines = vec![
            format!("Turn control projection surface: {}", self.surface.as_str()),
            format!("Projection ID: {}", self.projection_id),
        ];
        if allowed_tools.is_empty() {
            lines.push("Allowed tools for this turn: none".to_string());
        } else {
            lines.push(format!(
                "Allowed tools for this turn: {}",
                allowed_tools.join(", ")
            ));
        }
        if !forbidden_tools.is_empty() {
            lines.push(format!(
                "Forbidden tools for this turn: {}",
                forbidden_tools.join(", ")
            ));
        }
        if let Some(action) = self.required_action.as_ref() {
            let action_label = action.projection_label();
            lines.push(format!("Required action: {action_label}"));
            if let Some(contract) = content_shape_projection_contract(action) {
                lines.push(contract);
            }
        }
        if !self.operation_intents.is_empty() {
            let intents = self
                .operation_intents
                .iter()
                .map(|intent| intent.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Operation intent: {intents}"));
            if self
                .operation_intents
                .contains(&OperationIntent::ContentChangingAuthoringRequired)
            {
                lines.push(content_changing_authoring_progress_guidance(
                    &self.allowed_tools,
                ));
            }
        }
        if !self.obligation_ids.is_empty() {
            lines.push(format!("Obligations: {}", self.obligation_ids.join(", ")));
        }
        if !self.contract_refs.is_empty() {
            lines.push(format!("Contract refs: {}", self.contract_refs.join(", ")));
        }
        if !self.evidence_refs.is_empty() {
            let refs = self
                .evidence_refs
                .iter()
                .map(|reference| format!("{}:{}", reference.source, reference.reference))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Evidence refs: {refs}"));
        }
        let body = body.into();
        if !body.trim().is_empty() {
            lines.push(body);
        }
        RenderedProjectionSurface {
            surface: self.surface,
            projection_id: self.projection_id,
            required_action: self.required_action.clone(),
            allowed_tools,
            forbidden_tools,
            operation_intents: self
                .operation_intents
                .iter()
                .map(|intent| intent.as_str().to_string())
                .collect(),
            text: lines.join("\n"),
        }
    }
}

fn content_shape_projection_contract(required_action: &RequiredAction) -> Option<String> {
    let target = required_action.edit_target()?;
    let target = target.as_str().trim();
    if target.is_empty() {
        return None;
    }
    let surface = required_action.tool.to_string();
    let content_subject = if surface == "apply_patch" {
        "`patch_text` must add or update the target with"
    } else {
        "`content` must be"
    };
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    if file_name.ends_with(".md")
        || file_name.ends_with(".markdown")
        || normalized.contains("/docs/")
        || matches!(
            file_name,
            "readme.md"
                | "design.md"
                | "basic_design.md"
                | "detail_design.md"
                | "detailed_design.md"
        )
    {
        return Some(format!(
            "Required positive text artifact shape for `{target}`: {content_subject} effective Markdown/text with real newline-separated document structure. Do not send a quote-wrapped whole-document string, JSON/Python-escaped serialized Markdown/text, or content dominated by literal `\\n` escape sequences instead of real newlines."
        ));
    }
    if file_name.starts_with("test_") || file_name.ends_with("_test.py") {
        return Some(format!(
            "Required positive test-module shape for `{target}`: {content_subject} executable test module text with real newlines, imports, test classes/functions, and assertions. Do not send production implementation code or quote-wrapped serialized source."
        ));
    }
    if file_name.ends_with(".py") {
        return Some(format!(
            "Required positive source shape for `{target}`: {content_subject} effective Python module text with real newline-separated source structure. Do not send quote-wrapped serialized source or literal `\\n` dominated content."
        ));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedProjectionSurface {
    pub surface: ProjectionSurfaceKind,
    pub projection_id: ProjectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_action: Option<RequiredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_intents: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionSurfaceKind {
    Prompt,
    ToolResultFeedback,
    RequestDiagnostics,
    Handoff,
    Preflight,
}

impl ProjectionSurfaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::ToolResultFeedback => "tool_result_feedback",
            Self::RequestDiagnostics => "request_diagnostics",
            Self::Handoff => "handoff",
            Self::Preflight => "preflight",
        }
    }
}

fn control_projection_body(surface: ProjectionSurfaceKind) -> &'static str {
    match surface {
        ProjectionSurfaceKind::Prompt => {
            "This prompt block is generated from the TurnControlEnvelope projection bundle. Follow the current tool surface and lifecycle state before any older reminder or transcript wording."
        }
        ProjectionSurfaceKind::ToolResultFeedback => {
            "This ToolResult feedback is generated from the TurnControlEnvelope projection bundle. The allowed tools list is availability metadata; satisfying recovery for content-changing authoring is the provider-visible edit primitive named in the same projection or equivalent artifact mutation evidence."
        }
        ProjectionSurfaceKind::RequestDiagnostics => {
            "This request diagnostics projection is generated from the TurnControlEnvelope projection bundle and must match the prompt, ToolResult feedback, handoff, and preflight surfaces."
        }
        ProjectionSurfaceKind::Handoff => {
            "This handoff projection is generated from the TurnControlEnvelope projection bundle. Continuation prose is subordinate to the typed lifecycle and tool surface shown here."
        }
        ProjectionSurfaceKind::Preflight => {
            "This preflight projection is generated from the TurnControlEnvelope projection bundle and is evaluated without relying on Live-LLM behavior."
        }
    }
}

fn sorted_tool_labels(tools: &[ToolName]) -> Vec<String> {
    let mut labels = tools.iter().map(ToString::to_string).collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

fn content_changing_authoring_progress_guidance(allowed_tools: &[ToolName]) -> String {
    let allowed = sorted_tool_labels(allowed_tools);
    let mut edit_tools = Vec::new();
    if allowed.iter().any(|tool| tool == "apply_patch") {
        edit_tools.push("apply_patch");
    }
    if allowed.iter().any(|tool| tool == "write") {
        edit_tools.push("write");
    }
    let edit_surface = if edit_tools.is_empty() {
        "equivalent file-change evidence".to_string()
    } else {
        format!(
            "{} or equivalent file-change evidence",
            edit_tools.join("/")
        )
    };
    let support_tools = allowed
        .iter()
        .filter(|tool| {
            matches!(
                tool.as_str(),
                "read"
                    | "list"
                    | "grep"
                    | "glob"
                    | "inspect_directory"
                    | "todowrite"
                    | "todo_write"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if support_tools.is_empty() {
        return format!(
            "Content-changing authoring progress requires workspace artifact edits from {edit_surface}. Do not answer with a text-only final message while requested artifacts remain open."
        );
    }
    format!(
        "Content-changing authoring progress requires workspace artifact edits from {edit_surface}. {} are supporting-only when available: they may gather context or update visible planning state, but they are not the satisfying progress surface for this operation intent.",
        support_tools.join("/")
    )
}

pub fn content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes()
-> bool {
    let projection_id = ProjectionId::new();
    let surface = ProjectionSurface {
        surface: ProjectionSurfaceKind::ToolResultFeedback,
        projection_id,
        required_action: None,
        allowed_tools: vec![
            ToolName::Read,
            ToolName::List,
            ToolName::TodoWrite,
            ToolName::Write,
            ToolName::ApplyPatch,
        ],
        forbidden_tools: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        obligation_ids: vec!["active_work".to_string()],
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
    };
    let text = surface.render_control_projection().text;
    text.contains("Allowed tools for this turn")
        && text.contains("supporting-only")
        && text.contains("not the satisfying progress surface")
        && text.contains("satisfying recovery")
        && text.contains("apply_patch/write or equivalent file-change evidence")
        && !text.contains("todowrite remain valid tool outputs")
        && !text.contains("current recovery surface")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DispatchPolicy {
    Dispatch,
    AwaitUser { reason: String },
    FailClosed { reason: String },
    Complete { reason: String },
    Interrupt { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub source: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ControlEnvelopeValidation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ControlEnvelopeIssue>,
}

impl ControlEnvelopeValidation {
    pub fn push_error(&mut self, code: ControlEnvelopeIssueCode, message: impl Into<String>) {
        self.issues.push(ControlEnvelopeIssue {
            code,
            severity: ControlEnvelopeIssueSeverity::Error,
            message: message.into(),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == ControlEnvelopeIssueSeverity::Error)
    }

    pub fn passes(&self) -> bool {
        !self.has_errors()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEnvelopeIssue {
    pub code: ControlEnvelopeIssueCode,
    pub severity: ControlEnvelopeIssueSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEnvelopeIssueSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEnvelopeIssueCode {
    SessionMismatch,
    ProjectionIdMismatch,
    AllowedSurfaceMismatch,
    ForbiddenSurfaceMismatch,
    RequiredActionMismatch,
    RequiredActionToolNotAllowed,
    RequiredToolChoiceWithoutTools,
    NamedToolChoiceNotAllowed,
    ToolCapabilityMissing,
    ImageCapabilityMissing,
    CompletionWithOpenObligations,
    DispatchWithoutSurface,
    DispatchWithoutOpenObligations,
    ObligationAuthorityMismatch,
}

fn validate_projection_id(
    validation: &mut ControlEnvelopeValidation,
    surface: &str,
    expected: ProjectionId,
    actual: ProjectionId,
) {
    if expected != actual {
        validation.push_error(
            ControlEnvelopeIssueCode::ProjectionIdMismatch,
            format!("{surface} projection_id differs from control envelope projection_id"),
        );
    }
}

fn validate_obligation_authority_alignment(
    validation: &mut ControlEnvelopeValidation,
    authority: &ActionAuthority,
    obligations: &ObligationSet,
) {
    let open_obligations = obligations
        .items
        .iter()
        .filter(|item| item.status.is_open())
        .collect::<Vec<_>>();
    if open_obligations.is_empty() {
        return;
    }

    for command in &authority.required_verification_commands {
        let has_command = open_obligations.iter().any(|item| {
            item.verification_commands
                .iter()
                .any(|obligation_command| obligation_command == command)
        });
        if !has_command {
            validation.push_error(
                ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
                format!(
                    "compiled open obligations do not include verification command `{command}`"
                ),
            );
        }
    }
}

fn validate_tool_choice(
    validation: &mut ControlEnvelopeValidation,
    tool_choice: &ToolChoice,
    allowed_tools: &[ToolName],
) {
    match tool_choice {
        ToolChoice::Required if allowed_tools.is_empty() => validation.push_error(
            ControlEnvelopeIssueCode::RequiredToolChoiceWithoutTools,
            "required tool_choice cannot be used with an empty allowed tool surface",
        ),
        ToolChoice::Named(tool) if !allowed_tools.contains(tool) => validation.push_error(
            ControlEnvelopeIssueCode::NamedToolChoiceNotAllowed,
            format!("named tool_choice `{tool}` is not in the allowed tool surface"),
        ),
        _ => {}
    }
}

fn compile_tool_choice(
    required_action: Option<&RequiredAction>,
    allowed_tools: &[ToolName],
    requested: ToolChoice,
) -> ToolChoice {
    if allowed_tools.is_empty() {
        return ToolChoice::None;
    }
    if matches!(requested, ToolChoice::Required | ToolChoice::Named(_)) {
        return requested;
    }
    if required_action.is_some_and(|action| action.tool != ToolName::ApplyPatch) {
        return ToolChoice::Required;
    }
    ToolChoice::Auto
}

pub fn verification_only_authority_narrows_to_exact_shell_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run the required verification command.".to_string(),
        active_targets: vec![Utf8PathBuf::from("docs/component-design.md")],
        operation_intents: Vec::new(),
        required_verification_commands: vec!["python -m unittest".to_string()],
        allowed_tools: vec![
            ToolName::List,
            ToolName::Read,
            ToolName::Shell,
            ToolName::Write,
        ],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace"),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        provider: "openai_compat".to_string(),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        access_mode: crate::config::AccessMode::AutoReview,
        sandbox: super::SandboxProfile::WorkspaceWrite,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_contract,
        allowed_tools: vec![
            ToolName::List,
            ToolName::Read,
            ToolName::Shell,
            ToolName::Write,
        ],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![
        TurnObligation {
            obligation_id: "active_work".to_string(),
            kind: ObligationKind::UserWork,
            summary: "Run the required verification command.".to_string(),
            targets: vec![Utf8PathBuf::from("docs/component-design.md")],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
        TurnObligation {
            obligation_id: "verification".to_string(),
            kind: ObligationKind::Verification,
            summary: "Required verification commands must run.".to_string(),
            targets: vec![Utf8PathBuf::from("docs/component-design.md")],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: vec!["python -m unittest".to_string()],
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
    ]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    authority.allowed_tools == vec![ToolName::Shell]
        && authority.required_action.as_ref().is_some_and(|action| {
            action.kind == RequiredActionKind::ShellCommand
                && action.tool == ToolName::Shell
                && action.target.is_none()
                && action.command.as_deref() == Some("python -X utf8 -m unittest")
                && action.projection_text == "shell:python -X utf8 -m unittest"
        })
        && authority
            .required_verification_commands
            .contains(&"python -m unittest".to_string())
        && authority.tool_choice == ToolChoice::Required
        && authority.required_action_is_allowed()
}

pub fn singleton_missing_target_stable_surface_projects_apply_patch_action_fixture_passes() -> bool
{
    let projection_id = ProjectionId::new();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary: "Requested deliverable is still missing: test_widget.py.".to_string(),
        active_targets: vec![Utf8PathBuf::from("test_widget.py")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec!["python -m unittest".to_string()],
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Shell, ToolName::TodoWrite],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace"),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        provider: "openai_compat".to_string(),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        access_mode: crate::config::AccessMode::AutoReview,
        sandbox: super::SandboxProfile::WorkspaceWrite,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Shell, ToolName::TodoWrite],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![
        TurnObligation {
            obligation_id: "active_work".to_string(),
            kind: ObligationKind::UserWork,
            summary: "Create the missing test artifact.".to_string(),
            targets: vec![Utf8PathBuf::from("test_widget.py")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
        TurnObligation {
            obligation_id: "verification".to_string(),
            kind: ObligationKind::Verification,
            summary: "Verification remains pending after authoring.".to_string(),
            targets: vec![Utf8PathBuf::from("test_widget.py")],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: vec!["python -m unittest".to_string()],
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
    ]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let bundle = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);
    let rendered = bundle.tool_result_feedback.render_control_projection().text;

    authority.allowed_tools == vec![ToolName::ApplyPatch, ToolName::Shell, ToolName::TodoWrite]
        && authority.required_action.as_ref().is_some_and(|action| {
            action.kind == RequiredActionKind::EditTarget
                && action.tool == ToolName::ApplyPatch
                && action.target.as_deref() == Some(camino::Utf8Path::new("test_widget.py"))
                && action.command.is_none()
                && action.projection_text == "apply_patch:test_widget.py"
        })
        && authority.tool_choice == ToolChoice::Auto
        && authority.required_action_is_allowed()
        && bundle
            .request_diagnostics
            .required_action
            .as_ref()
            .is_some_and(|action| action.tool == ToolName::ApplyPatch)
        && rendered.contains("Required action: apply_patch:test_widget.py")
        && rendered.contains("patch_text")
        && rendered.contains("test_widget.py")
        && rendered.contains("not the satisfying progress surface")
        && !rendered.contains("Use the `write` tool")
        && !rendered.contains("tool_choice")
}

fn validate_model_capabilities(
    validation: &mut ControlEnvelopeValidation,
    capabilities: &ModelCapabilities,
    image_turn: bool,
    allowed_tools: &[ToolName],
    tool_choice: &ToolChoice,
) {
    if !capabilities.supports_tools
        && (!allowed_tools.is_empty() || !matches!(tool_choice, ToolChoice::None))
    {
        validation.push_error(
            ControlEnvelopeIssueCode::ToolCapabilityMissing,
            "tool surface cannot be dispatched to a model that does not support tools",
        );
    }
    if image_turn && !capabilities.supports_images {
        validation.push_error(
            ControlEnvelopeIssueCode::ImageCapabilityMissing,
            "image turn cannot be dispatched to a non-vision model",
        );
    }
}

fn same_tool_set(left: &[ToolName], right: &[ToolName]) -> bool {
    left.iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        == right
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
}

fn tool_set_is_subset(left: &[ToolName], right: &[ToolName]) -> bool {
    left.iter().all(|tool| right.contains(tool))
}
