use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::{
    OperationIntent, ProjectionId, ToolChoice, TurnContext, TurnControlEnvelopeId, TurnId,
};
use crate::tool::ToolName;

/// Legacy protocol payload retained only so old sessions can deserialize and
/// export historical control-envelope items. The Phase14 agent loop never
/// creates this type for new runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnControlEnvelope {
    pub id: TurnControlEnvelopeId,
    pub session_id: crate::session::SessionId,
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
        Self {
            id: TurnControlEnvelopeId::new(),
            session_id: context.session_id,
            turn_id,
            projection_id: action_authority.projection_id,
            context,
            obligations,
            action_authority,
            projection_bundle,
            dispatch_policy,
            evidence_refs,
        }
    }

    pub fn validate(&self) -> ControlEnvelopeValidation {
        ControlEnvelopeValidation::default()
    }

    pub fn fail_closed_before_dispatch(&self) -> Option<&str> {
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

impl ActionAuthority {
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
        let allowed_tools = self
            .allowed_tools
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let forbidden_tools = self
            .forbidden_tools
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
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
            text: format!(
                "Legacy control projection surface: {}",
                self.surface.as_str()
            ),
        }
    }
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
