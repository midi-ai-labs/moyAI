use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use tokio_util::sync::CancellationToken;

use crate::cli::{ConfirmationOutcome, ConfirmationPrompt};
use crate::config::{AccessMode, ResolvedConfig};
use crate::edit::{
    ChangeTracker, EditSafety, Formatter, FormatterExecutionOptions, ResolvedFormatterInvocation,
};
use crate::error::ToolError;
use crate::protocol::{ToolApprovalDecision, TurnId};
use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl};
use crate::session::{AdmissionId, SessionContext, SessionId, SessionRepository, ToolCallId};
use crate::storage::{SqliteSessionRepository, session_repo::RunAdmissionLeaseRenewalOutcome};
use crate::storage::{StoragePaths, StoreBundle};
use crate::tool::permission_guardian::{
    PermissionGuardian, PermissionGuardianDecision, PermissionGuardianEvidenceState,
};
use crate::tool::truncate::ToolTruncator;
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace};

#[derive(Clone)]
pub struct ToolServices {
    pub edit_safety: EditSafety,
    pub formatter: Formatter,
    pub change_tracker: ChangeTracker,
    pub store: StoreBundle,
    pub storage_paths: StoragePaths,
    pub truncator: ToolTruncator,
    pub mcp: Arc<crate::mcp::McpClient>,
    pub skills: crate::skill::SkillsService,
}

pub struct ToolContext<'a> {
    pub session: &'a SessionContext,
    pub workspace: &'a Workspace,
    pub config: &'a ResolvedConfig,
    pub tool_call_id: ToolCallId,
    pub cancel: CancellationToken,
    pub run_control: RunControl,
    pub run_mutation_fence: RunMutationFence,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
    pub agent: Option<&'a crate::app::AgentRunContext>,
    pub permission_guardian: Option<&'a mut dyn PermissionGuardian>,
}

#[derive(Debug, Clone)]
pub struct ToolFormatterPlan {
    invocation: ResolvedFormatterInvocation,
    target_guard: GuardedPath,
    working_directory_guard: GuardedPath,
}

impl ToolFormatterPlan {
    pub fn resolve(
        config: &ResolvedConfig,
        workspace: &Workspace,
        target_guard: &GuardedPath,
    ) -> Result<Option<Self>, ToolError> {
        let Some(invocation) =
            Formatter::resolve_invocation(&config.format, &target_guard.absolute, &workspace.root)?
        else {
            return Ok(None);
        };
        let working_directory_guard =
            PathGuard::require_path(workspace, invocation.working_directory(), AccessKind::Shell)?;
        Ok(Some(Self {
            invocation,
            target_guard: target_guard.clone(),
            working_directory_guard,
        }))
    }

    pub fn permission_detail(&self) -> String {
        self.invocation.permission_detail()
    }

    pub fn target(&self) -> &Utf8Path {
        self.invocation.target()
    }

    pub fn command(&self) -> &[String] {
        self.invocation.command()
    }
}

#[must_use = "call admit immediately before every independently startable observable effect"]
#[derive(Clone)]
pub struct ToolEffectAdmission {
    control: RunControl,
}

impl ToolEffectAdmission {
    pub(crate) fn new(control: RunControl) -> Self {
        Self { control }
    }

    /// Linearizes one observable tool effect against Stop, Abort, failure, and supersession.
    /// Multi-stage tools reuse the same approved ticket before every independently startable
    /// effect so a later formatter, process, network request, or mutation cannot start after a
    /// terminal producer wins.
    pub fn admit(&self) -> Result<(), ToolError> {
        self.control
            .begin_tool_effect_admission()
            .ok_or(ToolError::RunInterrupted)?
            .admit()
            .map_err(|_| ToolError::RunInterrupted)
    }

    pub async fn format_if_planned(
        &self,
        formatter: &Formatter,
        plan: Option<&ToolFormatterPlan>,
        normalized: String,
        options: FormatterExecutionOptions,
    ) -> Result<String, ToolError> {
        let Some(plan) = plan else {
            return Ok(normalized);
        };
        PathGuard::revalidate(&plan.target_guard)?;
        PathGuard::revalidate(&plan.working_directory_guard)?;
        self.admit()?;
        formatter
            .format_resolved(&plan.invocation, normalized, options)
            .await
            .map_err(ToolError::from)
    }
}

#[derive(Clone)]
pub struct RunMutationFence {
    repo: SqliteSessionRepository,
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    control: RunControl,
}

impl RunMutationFence {
    pub fn new(
        repo: SqliteSessionRepository,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
        control: RunControl,
    ) -> Self {
        Self {
            repo,
            session_id,
            admission_id,
            turn_id,
            control,
        }
    }

    pub async fn assert_owned(&self) -> Result<(), ToolError> {
        if self.control.is_cancelled() {
            return Err(self.rejected_error("the run is cancelled"));
        }
        let outcome = match self
            .repo
            .renew_admitted_run_lease(self.session_id, self.admission_id, self.turn_id)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.control.fail(error.to_string());
                return Err(ToolError::Storage(error));
            }
        };
        match outcome {
            RunAdmissionLeaseRenewalOutcome::Renewed => {}
            RunAdmissionLeaseRenewalOutcome::Terminal(_) => {
                self.control.supersede();
                return Err(self.rejected_error("the admitted turn is already terminal"));
            }
            RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
                self.control.supersede();
                return Err(
                    self.rejected_error("the admission was superseded or its lease expired")
                );
            }
        }
        if self.control.is_cancelled() {
            return Err(self.rejected_error("the run was cancelled while checking ownership"));
        }
        Ok(())
    }

    fn rejected_error(&self, reason: &str) -> ToolError {
        ToolError::Message(format!(
            "run mutation rejected for session {} admission {} turn {} because {reason}",
            self.session_id, self.admission_id, self.turn_id
        ))
    }

    pub fn begin_effect_commit(
        &self,
    ) -> Result<crate::runtime::ToolEffectCommitReservation, ToolError> {
        self.control
            .begin_tool_effect_commit()
            .ok_or(ToolError::RunInterrupted)
    }
}

impl<'a> ToolContext<'a> {
    pub async fn confirm_if_needed(
        &mut self,
        access: AccessKind,
        summary: String,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<crate::tool::PermissionRisk>,
    ) -> Result<ToolEffectAdmission, ToolError> {
        self.confirm_if_needed_with_details(
            access,
            summary,
            Vec::new(),
            targets,
            outside_workspace,
            risks,
        )
        .await
    }

    pub async fn confirm_if_needed_with_details(
        &mut self,
        access: AccessKind,
        summary: String,
        details: Vec<String>,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<crate::tool::PermissionRisk>,
    ) -> Result<ToolEffectAdmission, ToolError> {
        self.confirm_if_needed_with_details_and_guardian_evidence(
            access,
            summary,
            details,
            targets,
            outside_workspace,
            risks,
            PermissionGuardianEvidenceState::permission_request(),
        )
        .await
    }

    pub async fn confirm_if_needed_with_details_and_guardian_evidence(
        &mut self,
        access: AccessKind,
        summary: String,
        details: Vec<String>,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<crate::tool::PermissionRisk>,
        guardian_evidence: PermissionGuardianEvidenceState,
    ) -> Result<ToolEffectAdmission, ToolError> {
        let request = crate::tool::PermissionRequest {
            access,
            summary,
            details,
            targets,
            outside_workspace,
            risks,
            agent_path: self
                .agent
                .filter(|agent| agent.is_sub_agent())
                .map(|agent| agent.path().to_string()),
            agent_task_name: self
                .agent
                .filter(|agent| agent.is_sub_agent())
                .map(|agent| agent.task_name().to_string()),
        };

        let access_mode = self.current_permission_access_mode().await?;
        if access_mode_allows_permission(access_mode, &request) {
            return self.accept_tool_effect();
        }

        if access_mode == AccessMode::AutoReview {
            let evidence = match &guardian_evidence {
                PermissionGuardianEvidenceState::Complete(evidence) => evidence,
                PermissionGuardianEvidenceState::Incomplete { reason } => {
                    return self.decline_permission(format!(
                        "automatic permission guardian was not given complete action evidence, so the action was blocked: {reason}. Do not retry it or an equivalent workaround without new user authorization"
                    ));
                }
            };
            let decision = match self.permission_guardian.as_deref_mut() {
                Some(guardian) => guardian.review(&request, evidence).await,
                None => {
                    return self.decline_permission(
                        "automatic permission guardian is unavailable; the action was blocked"
                            .to_string(),
                    );
                }
            };
            if self.run_control.is_cancelled() {
                return Err(ToolError::RunInterrupted);
            }
            return match decision {
                Ok(PermissionGuardianDecision::Allow { .. }) => self.accept_tool_effect(),
                Ok(PermissionGuardianDecision::Deny { rationale }) => self.decline_permission(
                    format!(
                        "automatic permission guardian denied the action: {rationale}. The action was not executed; do not retry it or an equivalent workaround without new user authorization"
                    ),
                ),
                Err(error) => self.decline_permission(format!(
                    "automatic permission guardian could not authorize the action, so it was blocked: {error}. Do not retry it or an equivalent workaround without new user authorization"
                )),
            };
        }

        let outcome = self
            .prompt
            .confirm_with_control(&request, &self.run_control)
            .map_err(|error| {
                let message = format!("failed to prompt for permission: {error}");
                self.run_control.fail(message.clone());
                ToolError::Message(message)
            })?;
        match outcome {
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved) => {
                self.accept_tool_effect()
            }
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { reason }) => {
                self.decline_permission(reason)
            }
            ConfirmationOutcome::AbortRequested => {
                let approval_abort = RunCancellationCause::Interruption(
                    crate::protocol::TurnInterruptionCause::ApprovalAborted,
                );
                let outcome = self.run_control.request_cancel(approval_abort.clone());
                if matches!(
                    outcome,
                    RunCancelOutcome::Applied | RunCancelOutcome::Deferred(_)
                ) {
                    Err(ToolError::PermissionAborted)
                } else {
                    Err(ToolError::RunInterrupted)
                }
            }
            ConfirmationOutcome::Aborted => Err(ToolError::PermissionAborted),
            ConfirmationOutcome::Interrupted => Err(ToolError::RunInterrupted),
        }
    }

    fn accept_tool_effect(&self) -> Result<ToolEffectAdmission, ToolError> {
        Ok(ToolEffectAdmission::new(self.run_control.clone()))
    }

    fn decline_permission(&self, reason: String) -> Result<ToolEffectAdmission, ToolError> {
        let settlement = self
            .run_control
            .begin_tool_settlement()
            .ok_or(ToolError::RunInterrupted)?;
        Err(ToolError::PermissionDenied {
            reason,
            settlement: Some(settlement),
        })
    }

    async fn current_permission_access_mode(&self) -> Result<AccessMode, ToolError> {
        let owner_session_id = self
            .agent
            .map(crate::app::AgentRunContext::root_session_id)
            .unwrap_or(self.session.session.id);
        Ok(self
            .services
            .store
            .session_repo()
            .get_session(owner_session_id)
            .await?
            .access_mode)
    }
}

pub fn access_mode_allows_permission(
    access_mode: AccessMode,
    request: &crate::tool::PermissionRequest,
) -> bool {
    match access_mode {
        AccessMode::FullAccess => true,
        AccessMode::Default | AccessMode::AutoReview => workspace_boundary_allows(request),
    }
}

fn workspace_boundary_allows(request: &crate::tool::PermissionRequest) -> bool {
    if request.outside_workspace || !request.risks.is_empty() {
        return false;
    }
    matches!(
        request.access,
        AccessKind::List | AccessKind::Search | AccessKind::Read | AccessKind::Edit
    )
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::protocol::ReviewDecision;
    use crate::session::{NewSession, ProjectId, ProjectRepository, SessionRepository};
    use crate::storage::{SqliteStore, StoragePaths};
    use crate::tool::permission_guardian::{PermissionGuardianDecision, PermissionGuardianError};
    use crate::workspace::AccessKind;

    #[derive(Default)]
    struct CountingPrompt {
        requests: usize,
    }

    impl ConfirmationPrompt for CountingPrompt {
        fn confirm(
            &mut self,
            _request: &crate::tool::PermissionRequest,
        ) -> Result<ReviewDecision, crate::error::CliPromptError> {
            self.requests += 1;
            Ok(ReviewDecision::Denied)
        }
    }

    #[derive(Clone, Copy)]
    enum FixedGuardianOutcome {
        Allow,
        Deny,
        Fail,
    }

    struct FixedGuardian {
        outcome: FixedGuardianOutcome,
        requests: usize,
    }

    #[async_trait::async_trait(?Send)]
    impl PermissionGuardian for FixedGuardian {
        async fn review(
            &mut self,
            _request: &crate::tool::PermissionRequest,
            _evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
        ) -> Result<PermissionGuardianDecision, PermissionGuardianError> {
            self.requests += 1;
            match self.outcome {
                FixedGuardianOutcome::Allow => Ok(PermissionGuardianDecision::Allow {
                    rationale: "scoped action".to_string(),
                }),
                FixedGuardianOutcome::Deny => Ok(PermissionGuardianDecision::Deny {
                    rationale: "not authorized".to_string(),
                }),
                FixedGuardianOutcome::Fail => Err(PermissionGuardianError::Request(
                    "fixture transport failure".to_string(),
                )),
            }
        }
    }

    async fn fence_test_session() -> (StoreBundle, SessionId) {
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
                title: "mutation fence".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        (store, session.id)
    }

    async fn permission_fixture(
        access_mode: AccessMode,
    ) -> (ResolvedConfig, SessionContext, ToolServices) {
        let (store, session_id) = fence_test_session().await;
        if access_mode != AccessMode::Default {
            store
                .session_repo()
                .compare_and_set_root_session_access_mode(
                    session_id,
                    AccessMode::Default,
                    access_mode,
                )
                .await
                .expect("access mode update")
                .expect("root access owner");
        }
        let session = store
            .session_repo()
            .get_session(session_id)
            .await
            .expect("session");
        let mut config = ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::Default;
        let workspace =
            crate::workspace::WorkspaceDiscovery::discover_fixed_root(&session.cwd, &config)
                .expect("workspace");
        let data_dir = session.cwd.clone();
        let services = ToolServices {
            edit_safety: EditSafety::default(),
            formatter: Formatter::new(config.format.clone()),
            change_tracker: ChangeTracker,
            store,
            storage_paths: StoragePaths {
                database_path: data_dir.join("moyai.sqlite3"),
                truncation_dir: data_dir.join("truncation"),
                data_dir,
            },
            truncator: ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
            skills: crate::skill::SkillsService::new(),
        };
        (config, SessionContext { session, workspace }, services)
    }

    fn permission(
        access: AccessKind,
        risks: Vec<crate::tool::PermissionRisk>,
    ) -> crate::tool::PermissionRequest {
        crate::tool::PermissionRequest {
            access,
            summary: "run shell".to_string(),
            details: Vec::new(),
            targets: vec![Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks,
            agent_path: None,
            agent_task_name: None,
        }
    }

    #[test]
    fn workspace_modes_review_shell_while_full_access_does_not() {
        let request = permission(AccessKind::Shell, Vec::new());

        let decisions = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ]
        .map(|mode| access_mode_allows_permission(mode, &request));
        assert_eq!(decisions, [false, false, true]);
    }

    #[test]
    fn full_access_never_creates_a_permission_prompt() {
        let request = permission(
            AccessKind::Shell,
            vec![crate::tool::PermissionRisk::ExternalConnection],
        );

        assert!(access_mode_allows_permission(
            AccessMode::FullAccess,
            &request
        ));
    }

    #[test]
    fn access_mode_policy_is_deterministic_for_risk_free_workspace_operations() {
        let cases = [
            (AccessKind::List, [true, true, true]),
            (AccessKind::Search, [true, true, true]),
            (AccessKind::Read, [true, true, true]),
            (AccessKind::Edit, [true, true, true]),
            (AccessKind::Shell, [false, false, true]),
        ];
        let modes = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ];

        for (access, expected) in cases {
            let request = permission(access, Vec::new());
            for (index, mode) in modes.into_iter().enumerate() {
                assert_eq!(
                    access_mode_allows_permission(mode, &request),
                    expected[index],
                    "unexpected {mode:?} decision for {access:?}"
                );
            }
        }
    }

    #[test]
    fn workspace_modes_keep_boundary_crossing_requests_for_review() {
        let hard_risks = [
            crate::tool::PermissionRisk::Network,
            crate::tool::PermissionRisk::ExternalConnection,
            crate::tool::PermissionRisk::ProtectedWorkspaceAuthority,
        ];

        for risk in hard_risks {
            let request = permission(AccessKind::Shell, vec![risk]);
            assert!(!access_mode_allows_permission(
                AccessMode::Default,
                &request
            ));
            assert!(!access_mode_allows_permission(
                AccessMode::AutoReview,
                &request
            ));
        }
        let mut outside = permission(AccessKind::Read, Vec::new());
        outside.outside_workspace = true;
        assert!(!access_mode_allows_permission(
            AccessMode::Default,
            &outside
        ));
        assert!(!access_mode_allows_permission(
            AccessMode::AutoReview,
            &outside
        ));
        assert!(access_mode_allows_permission(
            AccessMode::FullAccess,
            &outside
        ));
    }

    #[test]
    fn configured_local_service_crosses_only_the_workspace_modes_boundary() {
        let request = permission(
            AccessKind::Read,
            vec![crate::tool::PermissionRisk::ConfiguredLocalService],
        );
        let decisions = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ]
        .map(|mode| access_mode_allows_permission(mode, &request));
        assert_eq!(decisions, [false, false, true]);
    }

    #[test]
    fn workspace_authority_crosses_only_the_workspace_modes_boundary() {
        let request = permission(
            AccessKind::Edit,
            vec![crate::tool::PermissionRisk::ProtectedWorkspaceAuthority],
        );
        let decisions = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ]
        .map(|mode| access_mode_allows_permission(mode, &request));

        assert_eq!(decisions, [false, false, true]);
        assert!(access_mode_allows_permission(
            AccessMode::FullAccess,
            &permission(AccessKind::Edit, Vec::new())
        ));
    }

    #[test]
    fn full_access_allows_external_effects_without_permission_confirmation() {
        for (access, risk) in [
            (AccessKind::Read, crate::tool::PermissionRisk::Network),
            (
                AccessKind::Read,
                crate::tool::PermissionRisk::ExternalConnection,
            ),
            (
                AccessKind::Read,
                crate::tool::PermissionRisk::ConfiguredLocalService,
            ),
            (
                AccessKind::Edit,
                crate::tool::PermissionRisk::ExternalMutation,
            ),
            (
                AccessKind::Edit,
                crate::tool::PermissionRisk::ExternalDestructiveOperation,
            ),
        ] {
            let request = permission(access, vec![risk]);
            assert!(access_mode_allows_permission(
                AccessMode::FullAccess,
                &request
            ));
        }
    }

    #[test]
    fn destructive_and_move_risks_expand_only_at_full_access() {
        let modes = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ];
        for risk in [
            crate::tool::PermissionRisk::DestructiveDelete,
            crate::tool::PermissionRisk::MoveOrRename,
        ] {
            let request = permission(AccessKind::Edit, vec![risk]);
            let decisions = modes.map(|mode| access_mode_allows_permission(mode, &request));
            assert_eq!(decisions, [false, false, true]);
        }
    }

    #[tokio::test]
    async fn permission_decision_reads_the_durable_root_mode_after_turn_admission() {
        let (config, session, services) = permission_fixture(AccessMode::Default).await;
        services
            .store
            .session_repo()
            .compare_and_set_root_session_access_mode(
                session.session.id,
                AccessMode::Default,
                AccessMode::FullAccess,
            )
            .await
            .expect("live access update")
            .expect("matching access owner");
        assert_eq!(config.permissions.access_mode, AccessMode::Default);
        assert_eq!(session.session.access_mode, AccessMode::Default);

        let control = RunControl::new();
        let mut prompt = CountingPrompt::default();
        let mut context = ToolContext {
            session: &session,
            workspace: &session.workspace,
            config: &config,
            tool_call_id: ToolCallId::new(),
            cancel: control.token(),
            run_control: control.clone(),
            run_mutation_fence: RunMutationFence::new(
                services.store.session_repo(),
                session.session.id,
                AdmissionId::new(),
                TurnId::new(),
                control,
            ),
            prompt: &mut prompt,
            services: &services,
            agent: None,
            permission_guardian: None,
        };
        let _ = context
            .confirm_if_needed(
                AccessKind::Shell,
                "run a command".to_string(),
                Vec::new(),
                false,
                Vec::new(),
            )
            .await
            .expect("full access from durable root owner");
        drop(context);
        assert_eq!(prompt.requests, 0);

        services
            .store
            .session_repo()
            .compare_and_set_root_session_access_mode(
                session.session.id,
                AccessMode::FullAccess,
                AccessMode::Default,
            )
            .await
            .expect("live access downgrade")
            .expect("matching access owner");
        let control = RunControl::new();
        let mut context = ToolContext {
            session: &session,
            workspace: &session.workspace,
            config: &config,
            tool_call_id: ToolCallId::new(),
            cancel: control.token(),
            run_control: control.clone(),
            run_mutation_fence: RunMutationFence::new(
                services.store.session_repo(),
                session.session.id,
                AdmissionId::new(),
                TurnId::new(),
                control,
            ),
            prompt: &mut prompt,
            services: &services,
            agent: None,
            permission_guardian: None,
        };
        assert!(matches!(
            context
                .confirm_if_needed(
                    AccessKind::Shell,
                    "run a second command".to_string(),
                    Vec::new(),
                    false,
                    Vec::new(),
                )
                .await,
            Err(ToolError::PermissionDenied { .. })
        ));
        drop(context);
        assert_eq!(prompt.requests, 1);
    }

    #[tokio::test]
    async fn auto_review_uses_guardian_as_final_decision_without_human_fallback() {
        for outcome in [
            FixedGuardianOutcome::Allow,
            FixedGuardianOutcome::Deny,
            FixedGuardianOutcome::Fail,
        ] {
            let (config, session, services) = permission_fixture(AccessMode::AutoReview).await;
            let control = RunControl::new();
            let mut prompt = CountingPrompt::default();
            let mut guardian = FixedGuardian {
                outcome,
                requests: 0,
            };
            let mut context = ToolContext {
                session: &session,
                workspace: &session.workspace,
                config: &config,
                tool_call_id: ToolCallId::new(),
                cancel: control.token(),
                run_control: control.clone(),
                run_mutation_fence: RunMutationFence::new(
                    services.store.session_repo(),
                    session.session.id,
                    AdmissionId::new(),
                    TurnId::new(),
                    control,
                ),
                prompt: &mut prompt,
                services: &services,
                agent: None,
                permission_guardian: Some(&mut guardian),
            };
            let result = context
                .confirm_if_needed(
                    AccessKind::Shell,
                    "run a command".to_string(),
                    Vec::new(),
                    false,
                    Vec::new(),
                )
                .await;
            drop(context);
            match guardian.outcome {
                FixedGuardianOutcome::Allow => assert!(result.is_ok()),
                FixedGuardianOutcome::Deny | FixedGuardianOutcome::Fail => {
                    assert!(matches!(result, Err(ToolError::PermissionDenied { .. })))
                }
            }
            assert_eq!(guardian.requests, 1);
            assert_eq!(prompt.requests, 0);
        }
    }

    #[tokio::test]
    async fn auto_review_fails_closed_before_guardian_when_action_evidence_is_incomplete() {
        let (config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let control = RunControl::new();
        let mut prompt = CountingPrompt::default();
        let mut guardian = FixedGuardian {
            outcome: FixedGuardianOutcome::Allow,
            requests: 0,
        };
        let mut context = ToolContext {
            session: &session,
            workspace: &session.workspace,
            config: &config,
            tool_call_id: ToolCallId::new(),
            cancel: control.token(),
            run_control: control.clone(),
            run_mutation_fence: RunMutationFence::new(
                services.store.session_repo(),
                session.session.id,
                AdmissionId::new(),
                TurnId::new(),
                control,
            ),
            prompt: &mut prompt,
            services: &services,
            agent: None,
            permission_guardian: Some(&mut guardian),
        };
        let result = context
            .confirm_if_needed_with_details_and_guardian_evidence(
                AccessKind::Shell,
                "run an incompletely represented action".to_string(),
                vec!["bounded human detail".to_string()],
                Vec::new(),
                false,
                Vec::new(),
                PermissionGuardianEvidenceState::incomplete(
                    "a sensitive executable field was redacted",
                ),
            )
            .await;
        drop(context);

        assert!(matches!(result, Err(ToolError::PermissionDenied { .. })));
        assert_eq!(guardian.requests, 0);
        assert_eq!(prompt.requests, 0);
    }

    #[tokio::test]
    async fn run_mutation_fence_rejects_cancelled_and_expired_owners_before_mutation() {
        let (store, session_id) = fence_test_session().await;
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repo
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admission")
            .expect("admitted")
            .admission_id;
        let control = RunControl::new();
        let fence = RunMutationFence::new(repo, session_id, admission_id, turn_id, control.clone());
        fence.assert_owned().await.expect("fresh owner");
        control.interrupt(crate::protocol::TurnInterruptionCause::UserStop);
        let mut cancelled_mutation_ran = false;
        if fence.assert_owned().await.is_ok() {
            cancelled_mutation_ran = true;
        }
        assert!(!cancelled_mutation_ran);

        let (expired_store, expired_session_id) = fence_test_session().await;
        let expired_repo = expired_store.session_repo();
        let expired_turn_id = TurnId::new();
        let expired_admission_id = expired_repo
            .admit_session_turn_at(expired_session_id, expired_turn_id, 0, 1)
            .await
            .expect("expired admission")
            .expect("admitted")
            .admission_id;
        let expired_control = RunControl::new();
        let expired_fence = RunMutationFence::new(
            expired_repo,
            expired_session_id,
            expired_admission_id,
            expired_turn_id,
            expired_control.clone(),
        );
        let mut expired_mutation_ran = false;
        if expired_fence.assert_owned().await.is_ok() {
            expired_mutation_ran = true;
        }
        assert!(!expired_mutation_ran);
        assert!(expired_control.is_cancelled());
    }
}
