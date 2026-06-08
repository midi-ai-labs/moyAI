use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::{
    ModelCapabilities, OperationIntent, ProjectionId, ToolChoice, TurnContext,
    TurnControlEnvelopeId, TurnId,
};
use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target as classify_language_artifact_target,
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
        validate_active_contract_context_lifecycle_alignment(&mut validation, &self.context);

        if !same_tool_set(
            &self.context.allowed_tools,
            &self.context.active_contract.allowed_tools,
        ) {
            validation.push_error(
                ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
                "turn context allowed tools differ from active contract allowed tools",
            );
        }
        validate_tool_surface_disjoint(
            &mut validation,
            "active work contract",
            &self.context.active_contract.allowed_tools,
            &self.context.active_contract.forbidden_tools,
        );

        if !tool_set_is_subset(
            &self.action_authority.allowed_tools,
            &self.context.allowed_tools,
        ) {
            validation.push_error(
                ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
                "action authority allowed tools must be compiled from the turn context allowed surface",
            );
        }
        validate_tool_surface_disjoint(
            &mut validation,
            "action authority",
            &self.action_authority.allowed_tools,
            &self.action_authority.forbidden_tools,
        );
        validate_action_authority_materialization(
            &mut validation,
            &self.action_authority,
            &ActionAuthority::from_obligations(
                &self.context,
                &self.obligations,
                self.context.tool_choice.clone(),
            ),
        );
        validate_active_contract_obligation_alignment(
            &mut validation,
            &self.context,
            &self.obligations,
        );
        validate_turn_decision_projection_alignment(
            &mut validation,
            &self.context,
            &self.action_authority,
        );
        validate_continuation_contract_alignment(&mut validation, &self.context);
        validate_output_contract_alignment(
            &mut validation,
            &self.context,
            &self.obligations,
            &self.dispatch_policy,
        );

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
            validate_tool_surface_disjoint(
                &mut validation,
                surface.surface.as_str(),
                &surface.allowed_tools,
                &surface.forbidden_tools,
            );
            validate_projection_surface_authority_alignment(
                &mut validation,
                surface,
                &ProjectionSurface::from_authority_and_obligations(
                    surface.surface,
                    &self.action_authority,
                    &self.obligations,
                ),
            );
        }

        validate_tool_choice(
            &mut validation,
            &self.action_authority.tool_choice,
            &self.action_authority.allowed_tools,
        );
        validate_tool_choice_required_action_alignment(
            &mut validation,
            &self.action_authority.tool_choice,
            self.action_authority.required_action.as_ref(),
        );
        validate_required_action_conflicts(&mut validation, &self.action_authority);
        validate_required_action_tool_surface(&mut validation, &self.action_authority);
        validate_repair_edit_surface_alignment(
            &mut validation,
            &self.context,
            &self.obligations,
            &self.action_authority,
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
    pub required_actions: Vec<RequiredAction>,
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
    pub required_action_conflicts: Vec<RequiredActionConflict>,
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
pub struct RequiredActionConflict {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligation_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<RequiredAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredAction {
    pub kind: RequiredActionKind,
    pub tool: ToolName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl RequiredAction {
    pub fn shell(command: String) -> Self {
        Self {
            kind: RequiredActionKind::ShellCommand,
            tool: ToolName::Shell,
            target: None,
            command: Some(command),
        }
    }

    pub fn edit(tool: ToolName, target: Utf8PathBuf) -> Self {
        Self {
            kind: RequiredActionKind::EditTarget,
            tool,
            target: Some(target),
            command: None,
        }
    }

    pub fn projection_label(&self) -> String {
        match self.kind {
            RequiredActionKind::ShellCommand => self
                .command
                .as_ref()
                .map(|command| format!("shell:{command}"))
                .unwrap_or_else(|| format!("{}:<missing-command>", self.tool)),
            RequiredActionKind::EditTarget => {
                let target = self
                    .target
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "<missing-target>".to_string());
                let prefix = match self.tool {
                    ToolName::ApplyPatch => "apply_patch",
                    ToolName::Write => "write",
                    _ => "edit",
                };
                format!("{prefix}:{target}")
            }
        }
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
        let parsed_required_actions = open_obligations
            .iter()
            .flat_map(|item| {
                item.required_actions
                    .iter()
                    .map(|action| (item.obligation_id.clone(), action.clone()))
            })
            .collect::<Vec<_>>();
        let mut explicit_obligation_ids = parsed_required_actions
            .iter()
            .map(|(obligation_id, _)| obligation_id.clone())
            .collect::<Vec<_>>();
        explicit_obligation_ids.sort();
        explicit_obligation_ids.dedup();
        let mut unique_explicit_actions = parsed_required_actions
            .iter()
            .map(|(_, action)| action.clone())
            .collect::<Vec<_>>();
        unique_explicit_actions.sort_by(|left, right| {
            left.projection_label()
                .cmp(&right.projection_label())
                .then_with(|| left.tool.to_string().cmp(&right.tool.to_string()))
        });
        unique_explicit_actions.dedup();
        let required_action_conflicts = if unique_explicit_actions.len() > 1 {
            vec![RequiredActionConflict {
                obligation_ids: explicit_obligation_ids,
                actions: unique_explicit_actions.clone(),
            }]
        } else {
            Vec::new()
        };
        let explicit_required_action = if required_action_conflicts.is_empty() {
            unique_explicit_actions.into_iter().next()
        } else {
            None
        };
        let required_action =
            if verification_command_only && required_verification_commands.len() == 1 {
                let executable = executable_verification_command_for_shell(
                    &required_verification_commands[0],
                    context.shell_family,
                );
                Some(RequiredAction::shell(executable))
            } else if !required_action_conflicts.is_empty() {
                None
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
        let mut forbidden_tools = context.active_contract.forbidden_tools.clone();
        if edit_only_authoring_grounding_recovery_obligation_active(context, &open_obligations) {
            let required_tool = required_action.as_ref().map(|action| action.tool.clone());
            let required_tool_is_available = required_tool
                .as_ref()
                .is_some_and(|required| allowed_tools.contains(required));
            let previous_allowed = allowed_tools.clone();
            allowed_tools.retain(|tool| {
                if required_tool_is_available {
                    required_tool
                        .as_ref()
                        .is_some_and(|required| tool == required && is_edit_tool(tool))
                } else {
                    is_edit_tool(tool)
                }
            });
            for tool in previous_allowed {
                if !allowed_tools.contains(&tool) && !forbidden_tools.contains(&tool) {
                    forbidden_tools.push(tool);
                }
            }
            for tool in edit_only_authoring_grounding_forbidden_surface() {
                if !allowed_tools.contains(&tool) && !forbidden_tools.contains(&tool) {
                    forbidden_tools.push(tool);
                }
            }
        }
        forbidden_tools.sort_by_key(|tool| tool.to_string());
        forbidden_tools.dedup();
        let tool_choice =
            compile_tool_choice(required_action.as_ref(), &allowed_tools, tool_choice);

        Self {
            projection_id: context.active_contract.projection_id,
            required_action,
            required_action_conflicts,
            required_verification_commands,
            operation_intents,
            allowed_tools,
            forbidden_tools,
            tool_choice,
        }
    }

    pub fn required_action_tool(&self) -> Option<ToolName> {
        self.required_action
            .as_ref()
            .map(|action| action.tool.clone())
    }

    pub fn required_action_is_allowed(&self) -> bool {
        self.required_action_tool()
            .is_none_or(|tool| self.allowed_tools.contains(&tool))
    }
}

fn edit_only_authoring_grounding_recovery_obligation_active(
    context: &TurnContext,
    open_obligations: &[&TurnObligation],
) -> bool {
    context.process_phase == crate::session::ProcessPhase::Repair
        && open_obligations.iter().any(|item| {
            item.contract_refs
                .iter()
                .any(|reference| reference == "authoring_target_grounding_recovery_edit_only")
                && item
                    .operation_intents
                    .contains(&OperationIntent::ContentChangingAuthoringRequired)
        })
}

fn edit_only_authoring_grounding_forbidden_surface() -> Vec<ToolName> {
    vec![
        ToolName::Read,
        ToolName::Shell,
        ToolName::TodoWrite,
        ToolName::Write,
    ]
}

fn is_edit_tool(tool: &ToolName) -> bool {
    matches!(tool, ToolName::ApplyPatch | ToolName::Write)
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
    let operation_template = edit_operation_template_projection(required_action, target);
    let target_spec = classify_language_artifact_target(target);
    let shape = match (target_spec.language, target_spec.role) {
        (_, ArtifactRole::Document) => {
            "effective Markdown/text with real newline-separated document structure. Do not send a quote-wrapped whole-document string, JSON-escaped serialized Markdown/text, or content dominated by literal `\\n` escape sequences instead of real newlines."
        }
        (LanguageFamily::Python, ArtifactRole::Test) => {
            "executable Python test module text with real newlines, imports, test classes/functions, and assertions. Do not send production implementation code or quote-wrapped serialized source."
        }
        (_, ArtifactRole::Test) => {
            "executable test artifact text with real newlines, imports or framework setup appropriate for the language, test functions/cases, and assertions. Do not send production implementation code or quote-wrapped serialized source."
        }
        (LanguageFamily::Python, ArtifactRole::Source) => {
            "effective Python module text with real newline-separated source structure. Do not send quote-wrapped serialized source or literal `\\n` dominated content."
        }
        (_, ArtifactRole::Source) => {
            "effective source artifact text with real newline-separated code structure appropriate for the language. Do not send quote-wrapped serialized source or literal `\\n` dominated content."
        }
        _ => {
            "effective workspace artifact text with real newline-separated structure appropriate for the target. Do not send quote-wrapped serialized content or literal `\\n` dominated content."
        }
    };
    Some(format!(
        "{operation_template}Required positive artifact shape for `{target}`: {content_subject} {shape}"
    ))
}

fn edit_operation_template_projection(required_action: &RequiredAction, target: &str) -> String {
    if required_action.tool != ToolName::ApplyPatch {
        return String::new();
    }
    format!(
        "Current patch operation template for `{target}`: use `*** Add File: {target}` if the active target is missing, or `*** Update File: {target}` if it already exists; the patch must touch only the active target. "
    )
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

pub fn active_apply_patch_target_projection_renders_operation_template_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let surface = ProjectionSurface {
        surface: ProjectionSurfaceKind::Prompt,
        projection_id,
        required_action: Some(RequiredAction::edit(
            ToolName::ApplyPatch,
            Utf8PathBuf::from("tests/workflow.behavior.md"),
        )),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Read],
        forbidden_tools: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        obligation_ids: vec!["active_work".to_string()],
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
    };
    let text = surface.render_control_projection().text;
    text.contains("Current patch operation template for `tests/workflow.behavior.md`")
        && text.contains("*** Add File: tests/workflow.behavior.md")
        && text.contains("*** Update File: tests/workflow.behavior.md")
        && text.contains("must touch only the active target")
        && !text.contains("*** Add File: src/workflow.rs")
}

pub fn projection_bundle_lifecycle_fields_match_authority_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("authoring_repair".to_string()),
        summary: "Repair exact target before verification.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch],
        forbidden_tools: vec![ToolName::Read, ToolName::Shell],
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "explicit_repair_action".to_string(),
        kind: ObligationKind::Repair,
        summary: "Explicit repair action requires apply_patch for the current target.".to_string(),
        targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: vec![RequiredAction::edit(ToolName::ApplyPatch, target)],
        verification_commands: Vec::new(),
        contract_refs: vec!["explicit_required_action_surface_authority".to_string()],
        evidence_refs: vec![EvidenceRef {
            source: "fixture".to_string(),
            reference: "current_authority".to_string(),
        }],
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let valid_bundle = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);
    let valid_envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context.clone(),
        obligations.clone(),
        authority.clone(),
        valid_bundle.clone(),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let mut stale_bundle = valid_bundle;
    stale_bundle.prompt.required_action = Some(RequiredAction::edit(
        ToolName::Write,
        Utf8PathBuf::from("stale/workflow.rs"),
    ));
    stale_bundle.prompt.operation_intents = Vec::new();
    stale_bundle.prompt.obligation_ids = vec!["stale_obligation".to_string()];
    stale_bundle.prompt.contract_refs = vec!["stale_contract".to_string()];
    stale_bundle.prompt.evidence_refs = vec![EvidenceRef {
        source: "fixture".to_string(),
        reference: "stale_projection".to_string(),
    }];
    let stale_envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations,
        authority,
        stale_bundle,
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let stale_validation = stale_envelope.validate();
    valid_envelope.validate().passes()
        && stale_validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::RequiredActionMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && stale_validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::ObligationAuthorityMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && stale_envelope.fail_closed_before_dispatch().is_some()
}

pub fn named_tool_choice_matches_required_action_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("authoring_repair".to_string()),
        summary: "Repair exact target before verification.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        tool_choice: ToolChoice::Named(ToolName::Write),
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "explicit_apply_patch_repair".to_string(),
        kind: ObligationKind::Repair,
        summary: "Explicit repair action requires apply_patch for the current target.".to_string(),
        targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: vec![RequiredAction::edit(ToolName::ApplyPatch, target)],
        verification_commands: Vec::new(),
        contract_refs: vec!["explicit_required_action_surface_authority".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.required_action.as_ref().is_some_and(|action| {
        action.tool == ToolName::ApplyPatch
            && action.target.as_deref() == Some(camino::Utf8Path::new("src/workflow.rs"))
    }) && authority.tool_choice == ToolChoice::Named(ToolName::Write)
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::RequiredActionMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn action_authority_matches_open_obligations_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("authoring_repair".to_string()),
        summary: "Repair exact target before verification.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
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
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "explicit_apply_patch_repair".to_string(),
        kind: ObligationKind::Repair,
        summary: "Explicit repair action requires apply_patch for the current target.".to_string(),
        targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: vec![RequiredAction::edit(ToolName::ApplyPatch, target.clone())],
        verification_commands: Vec::new(),
        contract_refs: vec!["explicit_required_action_surface_authority".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let stale_authority = ActionAuthority {
        projection_id,
        required_action: Some(RequiredAction::edit(ToolName::Write, target)),
        required_action_conflicts: Vec::new(),
        required_verification_commands: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        tool_choice: ToolChoice::Required,
    };
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        stale_authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&stale_authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    validation.issues.iter().any(|issue| {
        issue.code == ControlEnvelopeIssueCode::RequiredActionMismatch
            && issue.severity == ControlEnvelopeIssueSeverity::Error
    }) && envelope.fail_closed_before_dispatch().is_some()
}

pub fn active_work_contract_matches_open_obligation_targets_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let active_target = Utf8PathBuf::from("src/workflow.rs");
    let obligation_target = Utf8PathBuf::from("stale/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("authoring_repair".to_string()),
        summary: "Repair exact target before verification.".to_string(),
        active_targets: vec![active_target],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch],
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch],
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
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "active_repair".to_string(),
        kind: ObligationKind::Repair,
        summary: "Open obligation targets a stale file.".to_string(),
        targets: vec![obligation_target],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: Vec::new(),
        verification_commands: Vec::new(),
        contract_refs: vec!["active_work_contract_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.required_action.as_ref().is_some_and(|action| {
        action.target.as_deref() == Some(camino::Utf8Path::new("stale/workflow.rs"))
    }) && validation.issues.iter().any(|issue| {
        issue.code == ControlEnvelopeIssueCode::ObligationAuthorityMismatch
            && issue.severity == ControlEnvelopeIssueSeverity::Error
    }) && envelope.fail_closed_before_dispatch().is_some()
}

pub fn verification_active_work_matches_open_obligation_targets_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let active_target = Utf8PathBuf::from("docs/workflow-design.md");
    let obligation_target = Utf8PathBuf::from("src/workflow.rs");
    let command = "verify-contract --behavior --encoding utf-8".to_string();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification for the current target.".to_string(),
        active_targets: vec![active_target],
        operation_intents: Vec::new(),
        required_verification_commands: vec![command.clone()],
        allowed_tools: vec![ToolName::Shell],
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
        allowed_tools: vec![ToolName::Shell],
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
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "verification".to_string(),
        kind: ObligationKind::Verification,
        summary: "Open verification obligation targets the actual source file.".to_string(),
        targets: vec![obligation_target],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: vec![command],
        contract_refs: vec!["verification_active_work_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority
        .required_verification_commands
        .contains(&"verify-contract --behavior --encoding utf-8".to_string())
        && authority.required_action.as_ref().is_some_and(|action| {
            action.kind == RequiredActionKind::ShellCommand && action.tool == ToolName::Shell
        })
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::ObligationAuthorityMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn active_work_contract_route_phase_matches_turn_context_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let command = "verify-contract --behavior --encoding utf-8".to_string();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Docs,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification for the current target.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: Vec::new(),
        required_verification_commands: vec![command.clone()],
        allowed_tools: vec![ToolName::Shell],
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
        allowed_tools: vec![ToolName::Shell],
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
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "verification".to_string(),
        kind: ObligationKind::Verification,
        summary: "Open verification obligation matches the context target and command.".to_string(),
        targets: vec![target],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: vec![command],
        contract_refs: vec!["active_contract_context_lifecycle_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority
        .required_verification_commands
        .contains(&"verify-contract --behavior --encoding utf-8".to_string())
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::LifecycleStateMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn allowed_forbidden_tool_surfaces_are_disjoint_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let command = "verify-contract --behavior --encoding utf-8".to_string();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification for the current target.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: Vec::new(),
        required_verification_commands: vec![command.clone()],
        allowed_tools: vec![ToolName::Shell],
        forbidden_tools: vec![ToolName::Shell],
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
        allowed_tools: vec![ToolName::Shell],
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
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "verification".to_string(),
        kind: ObligationKind::Verification,
        summary: "Open verification obligation matches the context target and command.".to_string(),
        targets: vec![target],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: vec![command],
        contract_refs: vec!["tool_surface_disjoint".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.allowed_tools.contains(&ToolName::Shell)
        && authority.forbidden_tools.contains(&ToolName::Shell)
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::ForbiddenSurfaceMismatch
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn turn_decision_projection_matches_control_envelope_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let stale_target = Utf8PathBuf::from("docs/stale-workflow.md");
    let command = "verify-contract --behavior --encoding utf-8".to_string();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification for the current target.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: Vec::new(),
        required_verification_commands: vec![command.clone()],
        allowed_tools: vec![ToolName::Shell],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let turn_decision_projection = crate::session::TurnDecisionDiagnostic {
        route: "docs".to_string(),
        process_phase: "author".to_string(),
        active_work_kind: Some("docs_authoring".to_string()),
        active_work_summary: Some("Write an old document.".to_string()),
        active_targets: vec![stale_target],
        verification_pending: false,
        closeout_ready: false,
        required_verification_commands: Vec::new(),
        policy_targets: Vec::new(),
        allowed_tools: vec!["write".to_string()],
        tool_choice: Some("auto".to_string()),
        warnings: Vec::new(),
        repair_lane: None,
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
        allowed_tools: vec![ToolName::Shell],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: Some(turn_decision_projection),
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "verification".to_string(),
        kind: ObligationKind::Verification,
        summary: "Open verification obligation matches the context target and command.".to_string(),
        targets: vec![target],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: vec![command],
        contract_refs: vec!["turn_decision_projection_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.allowed_tools == vec![ToolName::Shell]
        && validation.issues.iter().any(|issue| {
            matches!(
                issue.code,
                ControlEnvelopeIssueCode::LifecycleStateMismatch
                    | ControlEnvelopeIssueCode::ObligationAuthorityMismatch
                    | ControlEnvelopeIssueCode::AllowedSurfaceMismatch
                    | ControlEnvelopeIssueCode::RequiredActionMismatch
            ) && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn continuation_contract_matches_control_envelope_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let command = "verify-contract --behavior --encoding utf-8".to_string();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification for the current target.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: Vec::new(),
        required_verification_commands: vec![command.clone()],
        allowed_tools: vec![ToolName::Shell],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let continuation = crate::session::ContinuationContract {
        route: "docs".to_string(),
        process_phase: "author".to_string(),
        active_work_kind: Some("docs_authoring".to_string()),
        active_work_summary: Some("Continue old docs authoring.".to_string()),
        target_files: vec![Utf8PathBuf::from("docs/stale-workflow.md")],
        verification_commands: Vec::new(),
        failure_kind: None,
        failure_summary: None,
        completion_blocker: Some("Old docs target remains open.".to_string()),
        invariant_refs: vec!["continuation_contract_alignment".to_string()],
        lifecycle_guard_snapshot_refs: Vec::new(),
        lifecycle_guard_snapshot_payload: None,
        lifecycle_guard_snapshot_metadata: BTreeMap::new(),
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
        allowed_tools: vec![ToolName::Shell],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: Some(continuation),
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "verification".to_string(),
        kind: ObligationKind::Verification,
        summary: "Open verification obligation matches the context target and command.".to_string(),
        targets: vec![target],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: vec![command],
        contract_refs: vec!["continuation_contract_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.allowed_tools == vec![ToolName::Shell]
        && validation.issues.iter().any(|issue| {
            matches!(
                issue.code,
                ControlEnvelopeIssueCode::LifecycleStateMismatch
                    | ControlEnvelopeIssueCode::ObligationAuthorityMismatch
            ) && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn output_contract_final_answer_matches_open_obligations_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary: "Create the active artifact before closeout.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch],
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
        allowed_tools: vec![ToolName::ApplyPatch],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: true,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "active_work".to_string(),
        kind: ObligationKind::UserWork,
        summary: "Open content-changing work must be completed before final answer.".to_string(),
        targets: vec![target],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: Vec::new(),
        verification_commands: Vec::new(),
        contract_refs: vec!["output_contract_obligation_alignment".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    validation.issues.iter().any(|issue| {
        issue.code == ControlEnvelopeIssueCode::OutputContractMismatch
            && issue.severity == ControlEnvelopeIssueSeverity::Error
    }) && envelope.fail_closed_before_dispatch().is_some()
}

pub fn non_python_edit_projection_uses_language_adapter_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let surface = ProjectionSurface {
        surface: ProjectionSurfaceKind::Prompt,
        projection_id,
        required_action: Some(RequiredAction::edit(
            ToolName::ApplyPatch,
            Utf8PathBuf::from("src/workflow.rs"),
        )),
        allowed_tools: vec![ToolName::ApplyPatch],
        forbidden_tools: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        obligation_ids: vec!["active_work".to_string()],
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
    };
    let text = surface.render_control_projection().text;
    text.contains("Required positive artifact shape for `src/workflow.rs`")
        && text.contains("effective source artifact text")
        && !text.contains("effective Python module text")
        && !text.contains("executable Python test module")
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
    RequiredActionConflict,
    RequiredActionMismatch,
    RequiredActionSurfaceMismatch,
    RequiredActionToolNotAllowed,
    RequiredToolChoiceWithoutTools,
    NamedToolChoiceNotAllowed,
    ToolCapabilityMissing,
    ImageCapabilityMissing,
    OutputContractMismatch,
    CompletionWithOpenObligations,
    DispatchWithoutSurface,
    DispatchWithoutOpenObligations,
    ObligationAuthorityMismatch,
    LifecycleStateMismatch,
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

fn validate_action_authority_materialization(
    validation: &mut ControlEnvelopeValidation,
    actual: &ActionAuthority,
    expected: &ActionAuthority,
) {
    if actual.required_action != expected.required_action {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionMismatch,
            "action authority required action differs from TurnContext and open obligations",
        );
    }
    if actual.required_action_conflicts != expected.required_action_conflicts {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionConflict,
            "action authority required action conflicts differ from open obligations",
        );
    }
    if actual.required_verification_commands != expected.required_verification_commands {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "action authority verification commands differ from TurnContext and open obligations",
        );
    }
    if actual.operation_intents != expected.operation_intents {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "action authority operation intents differ from open obligations",
        );
    }
    if !same_tool_set(&actual.allowed_tools, &expected.allowed_tools) {
        validation.push_error(
            ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
            "action authority allowed tools differ from compiled TurnContext and open obligations",
        );
    }
    if !same_tool_set(&actual.forbidden_tools, &expected.forbidden_tools) {
        validation.push_error(
            ControlEnvelopeIssueCode::ForbiddenSurfaceMismatch,
            "action authority forbidden tools differ from compiled TurnContext and open obligations",
        );
    }
    if actual.tool_choice != expected.tool_choice {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionMismatch,
            "action authority tool_choice differs from compiled TurnContext and open obligations",
        );
    }
}

fn validate_tool_surface_disjoint(
    validation: &mut ControlEnvelopeValidation,
    surface: &str,
    allowed_tools: &[ToolName],
    forbidden_tools: &[ToolName],
) {
    let overlap = allowed_tools
        .iter()
        .filter(|tool| forbidden_tools.contains(tool))
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    if !overlap.is_empty() {
        validation.push_error(
            ControlEnvelopeIssueCode::ForbiddenSurfaceMismatch,
            format!(
                "{surface} allowed and forbidden tool surfaces overlap: {}",
                overlap.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

fn validate_active_contract_context_lifecycle_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
) {
    if context.active_contract.route != context.route {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "active work contract route differs from turn context route",
        );
    }
    if context.active_contract.process_phase != context.process_phase {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "active work contract process_phase differs from turn context process_phase",
        );
    }
}

fn validate_turn_decision_projection_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
    authority: &ActionAuthority,
) {
    let Some(decision) = context.turn_decision_projection.as_ref() else {
        return;
    };
    if decision.route != task_route_label(context.route) {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "turn decision projection route differs from turn context route",
        );
    }
    if decision.process_phase != process_phase_label(context.process_phase) {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "turn decision projection process_phase differs from turn context process_phase",
        );
    }
    if decision.active_work_kind != context.active_contract.active_work_kind {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "turn decision projection active_work_kind differs from active work contract",
        );
    }
    let mut decision_targets =
        canonicalize_workspace_targets(&decision.active_targets, &context.workspace_root);
    decision_targets.sort();
    decision_targets.dedup();
    let mut active_targets = canonicalize_workspace_targets(
        &context.active_contract.active_targets,
        &context.workspace_root,
    );
    active_targets.sort();
    active_targets.dedup();
    if decision_targets != active_targets {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "turn decision projection active targets differ from active work contract",
        );
    }
    let mut decision_commands = decision.required_verification_commands.clone();
    decision_commands.sort();
    decision_commands.dedup();
    let mut active_commands = context
        .active_contract
        .required_verification_commands
        .clone();
    active_commands.sort();
    active_commands.dedup();
    if decision_commands != active_commands {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "turn decision projection verification commands differ from active work contract",
        );
    }
    let mut decision_tools = decision.allowed_tools.clone();
    decision_tools.sort();
    decision_tools.dedup();
    if decision_tools != sorted_tool_labels(&authority.allowed_tools) {
        validation.push_error(
            ControlEnvelopeIssueCode::AllowedSurfaceMismatch,
            "turn decision projection allowed tools differ from action authority",
        );
    }
    if decision.tool_choice.as_deref() != Some(tool_choice_projection_label(&authority.tool_choice))
    {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionMismatch,
            "turn decision projection tool_choice differs from action authority",
        );
    }
}

fn validate_continuation_contract_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
) {
    let Some(continuation) = context.continuation.as_ref() else {
        return;
    };
    if continuation.route != task_route_label(context.route) {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "continuation contract route differs from turn context route",
        );
    }
    if continuation.process_phase != process_phase_label(context.process_phase) {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "continuation contract process_phase differs from turn context process_phase",
        );
    }
    if continuation.active_work_kind != context.active_contract.active_work_kind {
        validation.push_error(
            ControlEnvelopeIssueCode::LifecycleStateMismatch,
            "continuation contract active_work_kind differs from active work contract",
        );
    }
    let mut continuation_targets =
        canonicalize_workspace_targets(&continuation.target_files, &context.workspace_root);
    continuation_targets.sort();
    continuation_targets.dedup();
    let mut active_targets = canonicalize_workspace_targets(
        &context.active_contract.active_targets,
        &context.workspace_root,
    );
    active_targets.sort();
    active_targets.dedup();
    if continuation_targets != active_targets {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "continuation contract targets differ from active work contract",
        );
    }
    let mut continuation_commands = continuation.verification_commands.clone();
    continuation_commands.sort();
    continuation_commands.dedup();
    let mut active_commands = context
        .active_contract
        .required_verification_commands
        .clone();
    active_commands.sort();
    active_commands.dedup();
    if continuation_commands != active_commands {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "continuation contract verification commands differ from active work contract",
        );
    }
}

fn validate_output_contract_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
    obligations: &ObligationSet,
    dispatch_policy: &DispatchPolicy,
) {
    let has_open_obligations = obligations.has_open_obligations();
    if has_open_obligations && context.output_contract.final_answer_required {
        validation.push_error(
            ControlEnvelopeIssueCode::OutputContractMismatch,
            "output contract requires final assistant text while obligations remain open",
        );
    }
    if has_open_obligations && context.output_contract.structured_schema_name.is_some() {
        validation.push_error(
            ControlEnvelopeIssueCode::OutputContractMismatch,
            "structured final output schema cannot be projected while obligations remain open",
        );
    }
    if matches!(dispatch_policy, DispatchPolicy::Complete { .. })
        && !context.output_contract.final_answer_required
    {
        validation.push_error(
            ControlEnvelopeIssueCode::OutputContractMismatch,
            "completion dispatch policy requires a final-answer output contract",
        );
    }
}

fn task_route_label(route: crate::session::TaskRoute) -> &'static str {
    match route {
        crate::session::TaskRoute::Code => "code",
        crate::session::TaskRoute::Docs => "docs",
        crate::session::TaskRoute::Review => "review",
        crate::session::TaskRoute::Debug => "debug",
        crate::session::TaskRoute::Ask => "ask",
        crate::session::TaskRoute::Summary => "summary",
    }
}

fn process_phase_label(phase: crate::session::ProcessPhase) -> &'static str {
    match phase {
        crate::session::ProcessPhase::Discover => "discover",
        crate::session::ProcessPhase::Author => "author",
        crate::session::ProcessPhase::Verify => "verify",
        crate::session::ProcessPhase::Repair => "repair",
        crate::session::ProcessPhase::Closeout => "closeout",
    }
}

fn tool_choice_projection_label(tool_choice: &ToolChoice) -> &'static str {
    match tool_choice {
        ToolChoice::Auto => "auto",
        ToolChoice::Required => "required",
        ToolChoice::None => "none",
        ToolChoice::Named(_) => "named",
    }
}

fn validate_active_contract_obligation_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
    obligations: &ObligationSet,
) {
    let open_obligations = obligations
        .items
        .iter()
        .filter(|item| item.status.is_open())
        .collect::<Vec<_>>();
    let open_content_obligations = obligations
        .items
        .iter()
        .filter(|item| {
            item.status.is_open()
                && item
                    .operation_intents
                    .contains(&OperationIntent::ContentChangingAuthoringRequired)
        })
        .collect::<Vec<_>>();
    let active_has_content_intent = context
        .active_contract
        .operation_intents
        .contains(&OperationIntent::ContentChangingAuthoringRequired);

    if active_has_content_intent != !open_content_obligations.is_empty() {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "active work contract operation intents differ from open obligations",
        );
    }

    if !open_content_obligations.is_empty() {
        let mut expected_intents = open_content_obligations
            .iter()
            .flat_map(|item| item.operation_intents.iter().copied())
            .collect::<Vec<_>>();
        expected_intents.sort_by_key(|intent| intent.as_str());
        expected_intents.dedup();
        let mut active_intents = context.active_contract.operation_intents.clone();
        active_intents.sort_by_key(|intent| intent.as_str());
        active_intents.dedup();
        if active_intents != expected_intents {
            validation.push_error(
                ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
                "active work contract operation intents are not compiled from open obligations",
            );
        }

        let mut expected_targets = open_content_obligations
            .iter()
            .flat_map(|item| {
                item.targets.iter().cloned().chain(
                    item.required_actions
                        .iter()
                        .filter_map(|action| action.edit_target().map(Utf8PathBuf::from)),
                )
            })
            .collect::<Vec<_>>();
        expected_targets =
            canonicalize_workspace_targets(&expected_targets, &context.workspace_root);
        expected_targets.sort();
        expected_targets.dedup();

        let mut active_targets = canonicalize_workspace_targets(
            &context.active_contract.active_targets,
            &context.workspace_root,
        );
        active_targets.sort();
        active_targets.dedup();
        if active_targets != expected_targets {
            validation.push_error(
                ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
                "active work contract targets differ from open content-changing obligations",
            );
        }
    }

    let mut active_verification_commands = context
        .active_contract
        .required_verification_commands
        .clone();
    active_verification_commands.sort();
    active_verification_commands.dedup();
    let open_verification_obligations = open_obligations
        .iter()
        .copied()
        .filter(|item| {
            !item.verification_commands.is_empty()
                && (item.kind == ObligationKind::Verification
                    || item
                        .verification_commands
                        .iter()
                        .any(|command| active_verification_commands.contains(command)))
        })
        .collect::<Vec<_>>();
    if active_verification_commands.is_empty() && open_verification_obligations.is_empty() {
        return;
    }

    let mut expected_verification_commands = open_verification_obligations
        .iter()
        .flat_map(|item| item.verification_commands.iter().cloned())
        .collect::<Vec<_>>();
    expected_verification_commands.sort();
    expected_verification_commands.dedup();
    if active_verification_commands != expected_verification_commands {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "active work contract verification commands differ from open verification obligations",
        );
    }

    let mut expected_verification_targets = open_verification_obligations
        .iter()
        .flat_map(|item| item.targets.iter().cloned())
        .collect::<Vec<_>>();
    expected_verification_targets =
        canonicalize_workspace_targets(&expected_verification_targets, &context.workspace_root);
    expected_verification_targets.sort();
    expected_verification_targets.dedup();
    let mut active_verification_targets = canonicalize_workspace_targets(
        &context.active_contract.active_targets,
        &context.workspace_root,
    );
    active_verification_targets.sort();
    active_verification_targets.dedup();
    if !active_verification_targets.is_empty()
        && !expected_verification_targets.is_empty()
        && active_verification_targets != expected_verification_targets
    {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            "active work contract targets differ from open verification obligations",
        );
    }
}

fn validate_projection_surface_authority_alignment(
    validation: &mut ControlEnvelopeValidation,
    surface: &ProjectionSurface,
    expected: &ProjectionSurface,
) {
    if surface.required_action != expected.required_action {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionMismatch,
            format!(
                "{} projection required action differs from action authority",
                surface.surface.as_str()
            ),
        );
    }
    if surface.operation_intents != expected.operation_intents {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            format!(
                "{} projection operation intents differ from action authority",
                surface.surface.as_str()
            ),
        );
    }
    if surface.obligation_ids != expected.obligation_ids {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            format!(
                "{} projection obligation ids differ from open obligations",
                surface.surface.as_str()
            ),
        );
    }
    if surface.contract_refs != expected.contract_refs {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            format!(
                "{} projection contract refs differ from open obligations",
                surface.surface.as_str()
            ),
        );
    }
    if surface.evidence_refs != expected.evidence_refs {
        validation.push_error(
            ControlEnvelopeIssueCode::ObligationAuthorityMismatch,
            format!(
                "{} projection evidence refs differ from open obligations",
                surface.surface.as_str()
            ),
        );
    }
}

fn validate_repair_edit_surface_alignment(
    validation: &mut ControlEnvelopeValidation,
    context: &TurnContext,
    obligations: &ObligationSet,
    authority: &ActionAuthority,
) {
    let open_obligations = obligations
        .items
        .iter()
        .filter(|item| item.status.is_open())
        .collect::<Vec<_>>();
    if !edit_only_authoring_grounding_recovery_obligation_active(context, &open_obligations) {
        return;
    }
    if let Some(required_tool) = authority.required_action_tool() {
        let non_required_tools = authority
            .allowed_tools
            .iter()
            .filter(|tool| **tool != required_tool)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !non_required_tools.is_empty() {
            validation.push_error(
                ControlEnvelopeIssueCode::RequiredActionSurfaceMismatch,
                format!(
                    "edit-only authoring grounding recovery requires the executable tool surface to match the required edit action; non-required tools remain allowed: {}",
                    non_required_tools.join(", ")
                ),
            );
        }
    }
    let non_edit_tools = authority
        .allowed_tools
        .iter()
        .filter(|tool| !is_edit_tool(tool))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if !non_edit_tools.is_empty() {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionSurfaceMismatch,
            format!(
                "edit-only authoring grounding recovery cannot expose supporting tools; non-edit tools remain allowed: {}",
                non_edit_tools.join(", ")
            ),
        );
    }
}

fn validate_required_action_tool_surface(
    validation: &mut ControlEnvelopeValidation,
    authority: &ActionAuthority,
) {
    let Some(required_action) = authority.required_action.as_ref() else {
        return;
    };
    if !authority.allowed_tools.contains(&required_action.tool) {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionToolNotAllowed,
            format!(
                "required action tool `{}` is not in the compiled allowed tool surface",
                required_action.tool
            ),
        );
    }
}

fn validate_required_action_conflicts(
    validation: &mut ControlEnvelopeValidation,
    authority: &ActionAuthority,
) {
    for conflict in &authority.required_action_conflicts {
        let actions = conflict
            .actions
            .iter()
            .map(RequiredAction::projection_label)
            .collect::<Vec<_>>()
            .join(", ");
        let obligations = if conflict.obligation_ids.is_empty() {
            "unknown obligations".to_string()
        } else {
            conflict.obligation_ids.join(", ")
        };
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionConflict,
            format!(
                "open obligations carry conflicting explicit required actions for {obligations}: {actions}"
            ),
        );
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

fn validate_tool_choice_required_action_alignment(
    validation: &mut ControlEnvelopeValidation,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) {
    let Some(required_action) = required_action else {
        return;
    };
    let ToolChoice::Named(named_tool) = tool_choice else {
        return;
    };
    if *named_tool != required_action.tool {
        validation.push_error(
            ControlEnvelopeIssueCode::RequiredActionMismatch,
            format!(
                "named tool_choice `{named_tool}` differs from required action tool `{}`",
                required_action.tool
            ),
        );
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
        active_targets: vec![Utf8PathBuf::from("docs/workflow-design.md")],
        operation_intents: Vec::new(),
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
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
            targets: vec![Utf8PathBuf::from("docs/workflow-design.md")],
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
            targets: vec![Utf8PathBuf::from("docs/workflow-design.md")],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: vec!["verify-contract --behavior --encoding utf-8".to_string()],
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
                && action.command.as_deref() == Some("verify-contract --behavior --encoding utf-8")
                && action.projection_label() == "shell:verify-contract --behavior --encoding utf-8"
        })
        && authority
            .required_verification_commands
            .contains(&"verify-contract --behavior --encoding utf-8".to_string())
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
        summary: "Requested deliverable is still missing: tests/workflow.behavior.md.".to_string(),
        active_targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
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
            targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
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
            targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: vec!["verify-contract --behavior --encoding utf-8".to_string()],
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
                && action.target.as_deref()
                    == Some(camino::Utf8Path::new("tests/workflow.behavior.md"))
                && action.command.is_none()
                && action.projection_label() == "apply_patch:tests/workflow.behavior.md"
        })
        && authority.tool_choice == ToolChoice::Auto
        && authority.required_action_is_allowed()
        && bundle
            .request_diagnostics
            .required_action
            .as_ref()
            .is_some_and(|action| action.tool == ToolName::ApplyPatch)
        && rendered.contains("Required action: apply_patch:tests/workflow.behavior.md")
        && rendered.contains("patch_text")
        && rendered.contains("tests/workflow.behavior.md")
        && rendered.contains("not the satisfying progress surface")
        && !rendered.contains("Use the `write` tool")
        && !rendered.contains("tool_choice")
}

pub fn conflicting_required_actions_fail_closed_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary: "Create the active artifact without conflicting lifecycle authority.".to_string(),
        active_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
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
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
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
            obligation_id: "source_patch_authority".to_string(),
            kind: ObligationKind::UserWork,
            summary: "Patch the active source artifact.".to_string(),
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: vec![RequiredAction::edit(
                ToolName::ApplyPatch,
                Utf8PathBuf::from("src/workflow.rs"),
            )],
            verification_commands: Vec::new(),
            contract_refs: vec!["explicit_required_action_authority".to_string()],
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
        TurnObligation {
            obligation_id: "source_write_authority".to_string(),
            kind: ObligationKind::Repair,
            summary: "Conflicting legacy write authority for the same item.".to_string(),
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: vec![RequiredAction::edit(
                ToolName::Write,
                Utf8PathBuf::from("src/workflow.rs"),
            )],
            verification_commands: Vec::new(),
            contract_refs: vec!["explicit_required_action_authority".to_string()],
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
    ]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let conflict_recorded = authority
        .required_action_conflicts
        .first()
        .is_some_and(|conflict| {
            conflict.obligation_ids
                == vec![
                    "source_patch_authority".to_string(),
                    "source_write_authority".to_string(),
                ]
                && conflict.actions.len() == 2
                && conflict
                    .actions
                    .iter()
                    .any(|action| action.projection_label() == "apply_patch:src/workflow.rs")
                && conflict
                    .actions
                    .iter()
                    .any(|action| action.projection_label() == "write:src/workflow.rs")
        });
    let no_fallback_action = authority.required_action.is_none();
    let bundle = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations,
        authority,
        bundle,
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    conflict_recorded
        && no_fallback_action
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::RequiredActionConflict
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch() == Some("control envelope validation failed")
}

pub fn turn_obligation_required_actions_are_typed_fixture_passes() -> bool {
    let obligation = TurnObligation {
        obligation_id: "typed_required_action_fixture".to_string(),
        kind: ObligationKind::UserWork,
        summary: "Required action must be typed at the obligation boundary.".to_string(),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: Vec::new(),
        verification_commands: Vec::new(),
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    };
    let required_actions_type = std::any::type_name_of_val(&obligation.required_actions);
    required_actions_type.contains("RequiredAction")
        && !required_actions_type.contains("alloc::string::String")
}

pub fn required_action_projection_label_is_typed_rendering_fixture_passes() -> bool {
    let serialized = serde_json::to_value(RequiredAction::edit(
        ToolName::ApplyPatch,
        Utf8PathBuf::from("src/workflow.rs"),
    ))
    .ok();
    RequiredAction::edit(ToolName::ApplyPatch, Utf8PathBuf::from("src/workflow.rs"))
        .projection_label()
        == "apply_patch:src/workflow.rs"
        && serialized
            .as_ref()
            .and_then(|value| value.as_object())
            .is_some_and(|object| !object.contains_key("projection_text"))
}

pub fn unavailable_explicit_required_action_fails_closed_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let target = Utf8PathBuf::from("src/workflow.rs");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("authoring_repair".to_string()),
        summary: "Repair exact target before verification.".to_string(),
        active_targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: Vec::new(),
        allowed_tools: vec![ToolName::ApplyPatch],
        forbidden_tools: vec![ToolName::Write],
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "explicit_repair_action".to_string(),
        kind: ObligationKind::Repair,
        summary: "Explicit repair action requires write for the current target.".to_string(),
        targets: vec![target.clone()],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: vec![RequiredAction::edit(ToolName::Write, target.clone())],
        verification_commands: Vec::new(),
        contract_refs: vec!["explicit_required_action_surface_authority".to_string()],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations.clone(),
        authority.clone(),
        ProjectionBundle::from_authority_and_obligations(&authority, &obligations),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let validation = envelope.validate();
    authority.required_action.as_ref().is_some_and(|action| {
        action.tool == ToolName::Write && action.target.as_deref() == Some(target.as_path())
    }) && !authority.required_action_is_allowed()
        && validation.issues.iter().any(|issue| {
            issue.code == ControlEnvelopeIssueCode::RequiredActionToolNotAllowed
                && issue.severity == ControlEnvelopeIssueSeverity::Error
        })
        && envelope.fail_closed_before_dispatch().is_some()
}

pub fn edit_only_authoring_grounding_recovery_narrows_action_surface_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("verification".to_string()),
        summary: "Repair src/workflow.rs before verification rerun.".to_string(),
        active_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
        allowed_tools: vec![
            ToolName::ApplyPatch,
            ToolName::Read,
            ToolName::Shell,
            ToolName::TodoWrite,
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![
            ToolName::ApplyPatch,
            ToolName::Read,
            ToolName::Shell,
            ToolName::TodoWrite,
            ToolName::Write,
        ],
        tool_choice: ToolChoice::Required,
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
            kind: ObligationKind::Verification,
            summary: "Repair exact source target before verification rerun.".to_string(),
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: vec!["verify-contract --behavior --encoding utf-8".to_string()],
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
        TurnObligation {
            obligation_id: "authoring_target_grounding_recovery".to_string(),
            kind: ObligationKind::Repair,
            summary: "Consumed targets: src/workflow.rs. Remaining read targets: none.".to_string(),
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs: vec![
                "authoring_target_grounding_recovery".to_string(),
                "authoring_target_grounding_recovery_edit_only".to_string(),
            ],
            evidence_refs: vec![EvidenceRef {
                source: "authoring_target_grounding".to_string(),
                reference: "active=src/workflow.rs;consumed=src/workflow.rs;missing=none"
                    .to_string(),
            }],
            status: ObligationStatus::Open,
        },
    ]);

    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let bundle = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);
    let envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context.clone(),
        obligations.clone(),
        authority.clone(),
        bundle,
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let bad_authority = ActionAuthority {
        projection_id,
        required_action: Some(RequiredAction::edit(
            ToolName::ApplyPatch,
            Utf8PathBuf::from("src/workflow.rs"),
        )),
        required_action_conflicts: Vec::new(),
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        allowed_tools: context.allowed_tools.clone(),
        forbidden_tools: Vec::new(),
        tool_choice: ToolChoice::Required,
    };
    let bad_bundle = ProjectionBundle::from_authority_and_obligations(&bad_authority, &obligations);
    let bad_envelope = TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations,
        bad_authority,
        bad_bundle,
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let apply_patch_only_context = TurnContext {
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
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract: super::ActiveWorkContractProjection {
            route: crate::session::TaskRoute::Code,
            process_phase: crate::session::ProcessPhase::Repair,
            active_work_kind: Some("verification".to_string()),
            summary: "Repair source before verification rerun.".to_string(),
            active_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_verification_commands: vec![
                "verify-contract --behavior --encoding utf-8".to_string(),
            ],
            allowed_tools: vec![ToolName::ApplyPatch],
            forbidden_tools: vec![ToolName::Read, ToolName::Shell, ToolName::TodoWrite],
            projection_id,
        },
        allowed_tools: vec![ToolName::ApplyPatch],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let write_alias_obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "authoring_target_grounding_recovery".to_string(),
        kind: ObligationKind::Repair,
        summary: "Consumed targets: src/workflow.rs. Remaining read targets: none.".to_string(),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: vec![RequiredAction::edit(
            ToolName::Write,
            Utf8PathBuf::from("src/workflow.rs"),
        )],
        verification_commands: Vec::new(),
        contract_refs: vec![
            "authoring_target_grounding_recovery".to_string(),
            "authoring_target_grounding_recovery_edit_only".to_string(),
        ],
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let write_alias_authority = ActionAuthority::from_obligations(
        &apply_patch_only_context,
        &write_alias_obligations,
        ToolChoice::Required,
    );
    let runtime_narrowed_authority = ActionAuthority::from_obligations(
        &apply_patch_only_context,
        &ObligationSet::new(vec![TurnObligation {
            obligation_id: "authoring_target_grounding_recovery".to_string(),
            kind: ObligationKind::Repair,
            summary: "Consumed targets: src/workflow.rs. Remaining read targets: none.".to_string(),
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs: vec![
                "authoring_target_grounding_recovery".to_string(),
                "authoring_target_grounding_recovery_edit_only".to_string(),
            ],
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        }]),
        ToolChoice::Required,
    );
    let multi_target_context = TurnContext {
        active_contract: super::ActiveWorkContractProjection {
            active_targets: vec![
                Utf8PathBuf::from("src/workflow.rs"),
                Utf8PathBuf::from("tests/workflow.behavior.md"),
            ],
            allowed_tools: vec![ToolName::ApplyPatch],
            forbidden_tools: Vec::new(),
            ..apply_patch_only_context.active_contract.clone()
        },
        allowed_tools: vec![ToolName::ApplyPatch],
        ..apply_patch_only_context.clone()
    };
    let multi_target_obligations = ObligationSet::new(vec![
        TurnObligation {
            obligation_id: "verification".to_string(),
            kind: ObligationKind::Verification,
            summary: "Verification remains pending after repair.".to_string(),
            targets: vec![
                Utf8PathBuf::from("src/workflow.rs"),
                Utf8PathBuf::from("tests/workflow.behavior.md"),
            ],
            operation_intents: Vec::new(),
            required_actions: Vec::new(),
            verification_commands: vec!["verify-contract --behavior --encoding utf-8".to_string()],
            contract_refs: Vec::new(),
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
        TurnObligation {
            obligation_id: "authoring_target_grounding_recovery".to_string(),
            kind: ObligationKind::Repair,
            summary:
                "Consumed targets: src/workflow.rs,tests/workflow.behavior.md. Remaining read targets: none."
                    .to_string(),
            targets: vec![
                Utf8PathBuf::from("src/workflow.rs"),
                Utf8PathBuf::from("tests/workflow.behavior.md"),
            ],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs: vec![
                "authoring_target_grounding_recovery".to_string(),
                "authoring_target_grounding_recovery_edit_only".to_string(),
            ],
            evidence_refs: Vec::new(),
            status: ObligationStatus::Open,
        },
    ]);
    let multi_target_authority = ActionAuthority::from_obligations(
        &multi_target_context,
        &multi_target_obligations,
        ToolChoice::Required,
    );
    let multi_target_envelope = TurnControlEnvelope::new(
        TurnId::new(),
        multi_target_context,
        multi_target_obligations.clone(),
        multi_target_authority.clone(),
        ProjectionBundle::from_authority_and_obligations(
            &multi_target_authority,
            &multi_target_obligations,
        ),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );

    let checks = [
        (
            "broad_allowed_narrowed",
            authority.allowed_tools == vec![ToolName::ApplyPatch],
        ),
        (
            "broad_forbidden_surface",
            authority.forbidden_tools
                == vec![
                    ToolName::Read,
                    ToolName::Shell,
                    ToolName::TodoWrite,
                    ToolName::Write,
                ],
        ),
        (
            "required_action_apply_patch",
            authority.required_action.as_ref().is_some_and(|action| {
                action.kind == RequiredActionKind::EditTarget
                    && action.tool == ToolName::ApplyPatch
                    && action.target.as_deref() == Some(camino::Utf8Path::new("src/workflow.rs"))
            }),
        ),
        ("envelope_validates", envelope.validate().passes()),
        (
            "bad_envelope_fails_surface",
            bad_envelope.validate().issues.iter().any(|issue| {
                issue.code == ControlEnvelopeIssueCode::RequiredActionSurfaceMismatch
                    && issue.severity == ControlEnvelopeIssueSeverity::Error
            }),
        ),
        (
            "projection_surfaces_narrowed",
            envelope
                .projection_bundle
                .surfaces()
                .iter()
                .all(|surface| surface.allowed_tools == vec![ToolName::ApplyPatch]),
        ),
        (
            "write_alias_allowed_narrowed",
            write_alias_authority.allowed_tools == vec![ToolName::ApplyPatch],
        ),
        (
            "write_alias_required_preserved",
            write_alias_authority
                .required_action
                .as_ref()
                .is_some_and(|action| {
                    action.tool == ToolName::Write
                        && action.target.as_deref()
                            == Some(camino::Utf8Path::new("src/workflow.rs"))
                }),
        ),
        (
            "write_alias_required_not_allowed",
            !write_alias_authority.required_action_is_allowed(),
        ),
        (
            "write_alias_fails_surface",
            TurnControlEnvelope::new(
                TurnId::new(),
                apply_patch_only_context.clone(),
                write_alias_obligations.clone(),
                write_alias_authority.clone(),
                ProjectionBundle::from_authority_and_obligations(
                    &write_alias_authority,
                    &write_alias_obligations,
                ),
                DispatchPolicy::Dispatch,
                Vec::new(),
            )
            .validate()
            .issues
            .iter()
            .any(|issue| {
                issue.code == ControlEnvelopeIssueCode::RequiredActionToolNotAllowed
                    && issue.severity == ControlEnvelopeIssueSeverity::Error
            }),
        ),
        (
            "runtime_narrowed_allowed",
            runtime_narrowed_authority.allowed_tools == vec![ToolName::ApplyPatch],
        ),
        (
            "runtime_narrowed_forbidden",
            runtime_narrowed_authority.forbidden_tools
                == vec![
                    ToolName::Read,
                    ToolName::Shell,
                    ToolName::TodoWrite,
                    ToolName::Write,
                ],
        ),
        (
            "multi_target_no_required_action",
            multi_target_authority.required_action.is_none(),
        ),
        (
            "multi_target_allowed",
            multi_target_authority.allowed_tools == vec![ToolName::ApplyPatch],
        ),
        (
            "multi_target_forbidden",
            multi_target_authority.forbidden_tools
                == vec![
                    ToolName::Read,
                    ToolName::Shell,
                    ToolName::TodoWrite,
                    ToolName::Write,
                ],
        ),
        (
            "multi_target_envelope_validates",
            multi_target_envelope.validate().passes(),
        ),
    ];
    checks.iter().all(|(_, passed)| *passed)
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
