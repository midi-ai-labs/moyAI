use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use super::{
    ModelCapabilities, OperationIntent, ProjectionId, ToolChoice, TurnContext,
    TurnControlEnvelopeId, TurnId,
};
use crate::session::SessionId;
use crate::tool::ToolName;

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
    pub required_next_action: Option<String>,
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
        let required_next_action =
            if verification_command_only && required_verification_commands.len() == 1 {
                Some(format!("shell:{}", required_verification_commands[0]))
            } else {
                None
            };
        let tool_choice =
            compile_tool_choice(required_next_action.as_deref(), &allowed_tools, tool_choice);

        Self {
            projection_id: context.active_contract.projection_id,
            required_next_action,
            required_verification_commands,
            operation_intents,
            allowed_tools,
            forbidden_tools: context.active_contract.forbidden_tools.clone(),
            tool_choice,
        }
    }

    pub fn required_action_tool(&self) -> Option<ToolName> {
        self.required_next_action
            .as_deref()
            .and_then(required_action_tool)
    }

    pub fn required_action_is_allowed(&self) -> bool {
        self.required_action_tool().is_none_or(|tool| {
            self.allowed_tools.contains(&tool)
                || (tool == ToolName::Write && self.allowed_tools.contains(&ToolName::ApplyPatch))
        })
    }
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
    pub required_next_action: Option<String>,
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
            required_next_action: authority.required_next_action.clone(),
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
                lines.push("Content-changing authoring progress requires workspace artifact edits from write/apply_patch or equivalent file-change evidence. Read/list/search and todowrite are supporting-only when available: they may gather context or update visible planning state, but they are not the satisfying progress surface for this operation intent.".to_string());
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
            required_next_action: self.required_next_action.clone(),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedProjectionSurface {
    pub surface: ProjectionSurfaceKind,
    pub projection_id: ProjectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_next_action: Option<String>,
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
            "This ToolResult feedback is generated from the TurnControlEnvelope projection bundle. The allowed tools list is availability metadata; satisfying recovery for content-changing authoring is file-change evidence from write/apply_patch or an equivalent artifact mutation."
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

pub fn content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes()
-> bool {
    let projection_id = ProjectionId::new();
    let surface = ProjectionSurface {
        surface: ProjectionSurfaceKind::ToolResultFeedback,
        projection_id,
        required_next_action: None,
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
        && text.contains("file-change evidence from write/apply_patch")
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
    required_next_action: Option<&str>,
    allowed_tools: &[ToolName],
    requested: ToolChoice,
) -> ToolChoice {
    if allowed_tools.is_empty() {
        return ToolChoice::None;
    }
    if matches!(requested, ToolChoice::Required | ToolChoice::Named(_)) {
        return requested;
    }
    if required_next_action.is_some() {
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
        active_targets: vec![Utf8PathBuf::from("docs/calculator-design.md")],
        operation_intents: Vec::new(),
        required_next_action: None,
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
            targets: vec![Utf8PathBuf::from("docs/calculator-design.md")],
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
            targets: vec![Utf8PathBuf::from("docs/calculator-design.md")],
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
        && authority.required_next_action.as_deref() == Some("shell:python -m unittest")
        && authority.tool_choice == ToolChoice::Required
        && authority.required_action_is_allowed()
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

fn required_action_tool(action: &str) -> Option<ToolName> {
    let tool = action
        .split_once(':')
        .map(|(tool, _)| tool)
        .unwrap_or(action)
        .trim()
        .trim_matches('`');
    match tool {
        "list" => Some(ToolName::List),
        "glob" => Some(ToolName::Glob),
        "grep" => Some(ToolName::Grep),
        "read" => Some(ToolName::Read),
        "inspect_directory" => Some(ToolName::InspectDirectory),
        "apply_patch" => Some(ToolName::ApplyPatch),
        "write" => Some(ToolName::Write),
        "shell" => Some(ToolName::Shell),
        "skill" => Some(ToolName::Skill),
        "docling_convert" => Some(ToolName::DoclingConvert),
        "mcp_call" => Some(ToolName::McpCall),
        "todowrite" => Some(ToolName::TodoWrite),
        _ => None,
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
