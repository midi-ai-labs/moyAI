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
use crate::tool::os_sandbox::ProcessSandboxPlan;
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
    pub managed_shells: crate::tool::shell::ManagedShells,
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
    shell: crate::config::ShellConfig,
    outside_workspace: bool,
    permission_risks: Vec<crate::tool::PermissionRisk>,
}

impl ToolFormatterPlan {
    pub fn resolve(
        config: &ResolvedConfig,
        workspace: &Workspace,
        target_guard: &GuardedPath,
    ) -> Result<Option<Self>, ToolError> {
        let Some(invocation) = Formatter::resolve_invocation(
            &config.format,
            &target_guard.absolute,
            workspace.authority_root(),
            &config.shell,
        )?
        else {
            return Ok(None);
        };
        let working_directory_guard =
            PathGuard::require_path(workspace, invocation.working_directory(), AccessKind::Shell)?;
        let permission_risks = crate::tool::shell::process_argv_permission_risks(
            workspace,
            &working_directory_guard,
            invocation.command(),
            &config.instructions.additional_files,
        );
        let outside_workspace = crate::tool::shell::process_argv_references_outside_workspace(
            workspace,
            &working_directory_guard,
            invocation.command(),
        );
        Ok(Some(Self {
            invocation,
            target_guard: target_guard.clone(),
            working_directory_guard,
            shell: config.shell.clone(),
            outside_workspace,
            permission_risks,
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

    pub fn permission_risks(&self) -> &[crate::tool::PermissionRisk] {
        &self.permission_risks
    }

    pub fn outside_workspace(&self) -> bool {
        self.outside_workspace
    }
}

pub(crate) fn targets_configured_instruction_authority(
    config: &ResolvedConfig,
    workspace: &Workspace,
    target: &Utf8Path,
) -> bool {
    config
        .instructions
        .additional_files
        .iter()
        .any(|configured| {
            let candidate = if configured.is_absolute() {
                configured.clone()
            } else {
                workspace.root.join(configured)
            };
            crate::workspace::project::normalize_path(&workspace.root, &candidate).is_ok_and(
                |candidate| {
                    PathGuard::stable_identity_key(&candidate)
                        == PathGuard::stable_identity_key(target)
                },
            )
        })
}

#[must_use = "call admit immediately before every independently startable observable effect"]
#[derive(Clone)]
pub struct ToolEffectAdmission {
    control: RunControl,
    sandbox_plan: ProcessSandboxPlan,
    permission_retry_lease: Option<crate::storage::PermissionReviewLease>,
    external_approval_id: Option<String>,
}

impl ToolEffectAdmission {
    pub(crate) fn new(control: RunControl, sandbox_plan: ProcessSandboxPlan) -> Self {
        let external_approval_id = control.take_approval_identity();
        Self {
            control,
            sandbox_plan,
            permission_retry_lease: None,
            external_approval_id,
        }
    }

    fn with_permission_retry_lease(
        mut self,
        lease: Option<crate::storage::PermissionReviewLease>,
    ) -> Self {
        self.permission_retry_lease = lease;
        self
    }

    pub(crate) fn sandbox_plan(&self) -> &ProcessSandboxPlan {
        &self.sandbox_plan
    }

    /// Settle the approval for a managed process after its actual startup is observed.
    /// The process keeps its sandbox and cancellation owner, but must not use this
    /// ticket to start another effect. Unlike Drop, handoff must confirm settlement.
    pub(crate) fn finish_started_effect(self) -> Result<(), ToolError> {
        let Some(lease) = &self.permission_retry_lease else {
            return Ok(());
        };
        match lease.release() {
            Ok(crate::storage::PermissionReviewTransition::Applied) => Ok(()),
            other => {
                let message = format!(
                    "起動した操作の承認記録を確定できないため、処理を停止しました: {other:?}"
                );
                self.control.fail(message.clone());
                Err(ToolError::Message(message))
            }
        }
    }

    /// Linearizes one observable tool effect against Stop, Abort, failure, and supersession.
    /// Multi-stage tools reuse the same approved ticket before every independently startable
    /// effect so a later formatter, process, network request, or mutation cannot start after a
    /// terminal producer wins.
    pub fn admit(&self) -> Result<(), ToolError> {
        self.control
            .authorize_external_effect(self.external_approval_id.as_deref())
            .map_err(|message| {
                if self.control.is_cancelled() {
                    return ToolError::RunInterrupted;
                }
                self.control.fail(message.clone());
                ToolError::Message(message)
            })?;
        if let Some(lease) = &self.permission_retry_lease {
            let admission = lease.admit_if_authority_current().map_err(|error| {
                let message = format!(
                    "automatic permission effect admission could not prove its durable retry-fence ownership: {error}"
                );
                self.control.fail(message.clone());
                ToolError::Message(message)
            })?;
            match admission {
                crate::storage::PermissionEffectAdmission::Admitted => {}
                crate::storage::PermissionEffectAdmission::AuthorityChanged {
                    current_authority_history_item_id,
                } => {
                    let Some(settlement) = self.control.begin_tool_settlement() else {
                        return Err(ToolError::RunInterrupted);
                    };
                    return Err(ToolError::PermissionDenied {
                        reason: format!(
                            "canonical user authorization changed before the approved effect could start (current generation: {current_authority_history_item_id:?})"
                        ),
                        settlement: Some(settlement),
                    });
                }
                crate::storage::PermissionEffectAdmission::NotOwned => {
                    let message =
                        "automatic permission effect admission lost its durable retry-fence ownership"
                            .to_string();
                    self.control.fail(message.clone());
                    return Err(ToolError::Message(message));
                }
            }
        }
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
            .format_resolved_with_sandbox(
                &plan.invocation,
                normalized,
                options,
                &plan.shell,
                &self.sandbox_plan,
            )
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
    pub(crate) fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// Bind a Hub origin to the exact durable local turn order. The revision is
    /// read while this turn still owns the session, so a later Stop can fence a
    /// delayed Hub admission without relying on wall-clock or ULID ordering.
    pub(crate) async fn origin_turn_revision(&self) -> Result<u64, ToolError> {
        self.assert_owned().await?;
        match self
            .repo
            .active_turn_expectation_for_session(self.session_id)
            .await?
        {
            Some(crate::session::ActiveTurnExpectation::Turn { turn_id, revision })
                if turn_id == self.turn_id =>
            {
                Ok(revision)
            }
            _ => Err(self.rejected_error("the active turn changed before Hub admission")),
        }
    }

    pub(crate) fn has_pending_turn_steer_input(&self) -> Result<bool, ToolError> {
        Ok(self.repo.has_pending_turn_steers_for_admitted_turn(
            self.session_id,
            self.admission_id,
            self.turn_id,
        )?)
    }

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
            RunAdmissionLeaseRenewalOutcome::InterruptRequested(cause) => {
                self.control
                    .request_cancel(RunCancellationCause::Interruption(cause));
                return Err(self.rejected_error("the exact execution interruption was requested"));
            }
            RunAdmissionLeaseRenewalOutcome::StopFenced(outcome) => {
                match outcome {
                    crate::protocol::TurnTerminalOutcome::Interrupted { cause } => {
                        self.control
                            .request_cancel(RunCancellationCause::Interruption(cause));
                    }
                    crate::protocol::TurnTerminalOutcome::Failed { error } => {
                        self.control
                            .request_cancel(RunCancellationCause::Failure(error));
                    }
                    crate::protocol::TurnTerminalOutcome::Completed => {
                        self.control.supersede();
                    }
                }
                return Err(self.rejected_error("the admitted turn is fenced for terminalization"));
            }
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
        let mut request = crate::tool::PermissionRequest {
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
            let sandbox_plan = process_sandbox_plan_for_admission(
                request.access,
                access_mode,
                self.workspace,
                self.config,
            )?;
            return self.accept_tool_effect(sandbox_plan, None);
        }

        if request.access == AccessKind::Shell {
            request.details.push(
                "execution boundary: approval grants this process effect elevation outside the workspace-write OS sandbox"
                    .to_string(),
            );
        }

        let mut human_review_lease = None;
        if access_mode == AccessMode::AutoReview {
            let evidence = match &guardian_evidence {
                PermissionGuardianEvidenceState::Complete(evidence) => evidence,
                PermissionGuardianEvidenceState::Incomplete { reason } => {
                    return self.fail_unfenced_auto_review(format!(
                        "automatic permission admission cannot establish a durable retry fence because action evidence is incomplete: {reason}"
                    ));
                }
            };
            let decision = match self.permission_guardian.as_deref_mut() {
                Some(guardian) => guardian.review(&request, evidence).await,
                None => {
                    return self.fail_unfenced_auto_review(
                        "automatic permission admission cannot establish a durable retry fence because the Guardian is unavailable"
                            .to_string(),
                    );
                }
            };
            if self.run_control.is_cancelled() {
                if let Some(lease) = self
                    .permission_guardian
                    .as_deref_mut()
                    .and_then(PermissionGuardian::take_retry_lease)
                {
                    self.require_owned_review_transition(lease.release())?;
                }
                return Err(ToolError::RunInterrupted);
            }
            let (reason, fence_outcome) = match decision {
                Ok(PermissionGuardianDecision::Allow { .. }) => {
                    let Some(retry_lease) = self
                        .permission_guardian
                        .as_deref_mut()
                        .and_then(PermissionGuardian::take_retry_lease)
                    else {
                        return self.fail_unfenced_auto_review(
                            "automatic permission Guardian allowed an elevated effect without an owned retry-fence lease"
                                .to_string(),
                        );
                    };
                    return self.accept_tool_effect(
                        approved_process_sandbox_plan(request.access),
                        Some(retry_lease),
                    );
                }
                Ok(PermissionGuardianDecision::AskUser { rationale }) => (
                    rationale,
                    crate::storage::PermissionRetryFenceOutcome::GuardianDenied,
                ),
                Ok(PermissionGuardianDecision::Deny { rationale }) => {
                    return self.stop_permission_work(format!(
                        "この操作は依頼の制限または禁止事項に反するため、実行せず停止しました。代理承認の判断: {rationale}。依頼内容を確認して、次の指示を入力してください。"
                    ));
                }
                Err(
                    crate::tool::permission_guardian::PermissionGuardianError::UnfencedAdmission(
                        reason,
                    ),
                ) => return self.fail_unfenced_auto_review(reason),
                Err(crate::tool::permission_guardian::PermissionGuardianError::RetryFenced(_)) => {
                    return self.stop_permission_work(
                        "この依頼には未解決または拒否済みの承認があるため、別の方法では実行せず停止しました。承認の結果を確認し、続ける場合は次の指示を入力してください。".to_string(),
                    );
                }
                Err(crate::tool::permission_guardian::PermissionGuardianError::Cancelled) => {
                    self.run_control
                        .interrupt(crate::protocol::TurnInterruptionCause::AgentInterrupted);
                    return Err(ToolError::RunInterrupted);
                }
                Err(crate::tool::permission_guardian::PermissionGuardianError::Request(_)) => (
                    "代理承認に使うAIに接続できないか、回答を受け取れませんでした。".to_string(),
                    crate::storage::PermissionRetryFenceOutcome::GuardianError,
                ),
                Err(
                    crate::tool::permission_guardian::PermissionGuardianError::InvalidDecision(_),
                ) => (
                    "代理承認に使うAIの回答から、許可の判断を確認できませんでした。".to_string(),
                    crate::storage::PermissionRetryFenceOutcome::InvalidDecision,
                ),
                Err(crate::tool::permission_guardian::PermissionGuardianError::TotalDeadline {
                    ..
                }) => (
                    "代理承認の待ち時間を超えたため、AIの判断を確認できませんでした。".to_string(),
                    crate::storage::PermissionRetryFenceOutcome::DeadlineExceeded,
                ),
            };
            let Some(lease) = self
                .permission_guardian
                .as_deref_mut()
                .and_then(PermissionGuardian::take_retry_lease)
            else {
                return self.fail_unfenced_auto_review(
                    "automatic permission handoff has no owned retry-fence lease".to_string(),
                );
            };
            request
                .details
                .push(format!("代理承認からの確認: {reason}"));
            request.details.push(
                "この操作はまだ実行していません。内容を確認して許可するか、拒否してください。拒否すると処理を停止し、次の指示を待ちます。".to_string(),
            );
            human_review_lease = Some((lease, fence_outcome));
        }

        let outcome = self
            .prompt
            .confirm_with_control_async(&request, &self.run_control)
            .await
            .map_err(|error| {
                let message = format!("failed to prompt for permission: {error}");
                self.run_control.fail(message.clone());
                ToolError::Message(message)
            })?;
        match outcome {
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved) => {
                let lease = if let Some((lease, _)) = human_review_lease {
                    self.require_owned_review_transition(lease.mark_allowed_pending())?;
                    Some(lease)
                } else {
                    None
                };
                self.accept_tool_effect(approved_process_sandbox_plan(request.access), lease)
            }
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { reason }) => {
                if let Some((lease, fence_outcome)) = human_review_lease {
                    self.require_owned_review_transition(lease.mark_denied(fence_outcome))?;
                    let result = self.decline_permission(format!(
                        "操作が拒否されたため実行せず停止しました。別の方法で続行せず、次の指示を待ちます。{reason}"
                    ));
                    self.run_control
                        .interrupt(crate::protocol::TurnInterruptionCause::ApprovalAborted);
                    result
                } else {
                    self.decline_permission(reason)
                }
            }
            ConfirmationOutcome::AbortRequested => {
                if let Some((lease, _)) = human_review_lease {
                    self.require_owned_review_transition(lease.release())?;
                }
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
            ConfirmationOutcome::Aborted | ConfirmationOutcome::Interrupted => {
                if let Some((lease, _)) = human_review_lease {
                    self.require_owned_review_transition(lease.release())?;
                }
                Err(if matches!(outcome, ConfirmationOutcome::Aborted) {
                    ToolError::PermissionAborted
                } else {
                    ToolError::RunInterrupted
                })
            }
        }
    }

    fn require_owned_review_transition(
        &self,
        transition: Result<crate::storage::PermissionReviewTransition, crate::error::StorageError>,
    ) -> Result<(), ToolError> {
        match transition {
            Ok(crate::storage::PermissionReviewTransition::Applied) => Ok(()),
            other => {
                let message = format!(
                    "代理承認から引き継いだ操作の承認記録を確認できないため、実行しませんでした: {other:?}"
                );
                self.run_control.fail(message.clone());
                Err(ToolError::Message(message))
            }
        }
    }

    fn stop_permission_work(&self, reason: String) -> Result<ToolEffectAdmission, ToolError> {
        let result = self.decline_permission(reason.clone());
        self.run_control.fail(reason);
        result
    }

    fn accept_tool_effect(
        &self,
        sandbox_plan: ProcessSandboxPlan,
        permission_retry_lease: Option<crate::storage::PermissionReviewLease>,
    ) -> Result<ToolEffectAdmission, ToolError> {
        Ok(
            ToolEffectAdmission::new(self.run_control.clone(), sandbox_plan)
                .with_permission_retry_lease(permission_retry_lease),
        )
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

    fn fail_unfenced_auto_review(&self, reason: String) -> Result<ToolEffectAdmission, ToolError> {
        self.run_control.fail(reason.clone());
        Err(ToolError::Message(reason))
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

fn process_sandbox_plan_for_admission(
    access: AccessKind,
    access_mode: AccessMode,
    workspace: &crate::workspace::Workspace,
    config: &ResolvedConfig,
) -> Result<ProcessSandboxPlan, crate::tool::os_sandbox::SandboxProfileError> {
    if access == AccessKind::Shell {
        ProcessSandboxPlan::for_access_mode_with_config(access_mode, workspace, config)
    } else {
        Ok(ProcessSandboxPlan::NoProcess)
    }
}

fn approved_process_sandbox_plan(access: AccessKind) -> ProcessSandboxPlan {
    if access == AccessKind::Shell {
        ProcessSandboxPlan::Unrestricted
    } else {
        ProcessSandboxPlan::NoProcess
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
        AccessKind::List
            | AccessKind::Search
            | AccessKind::Read
            | AccessKind::Edit
            | AccessKind::Shell
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
    use crate::workspace::{AccessKind, WorkspaceDiscovery};

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
        AskUser,
        Deny,
        Fail,
    }

    struct FixedGuardian {
        outcome: FixedGuardianOutcome,
        requests: usize,
    }

    #[derive(Default)]
    struct CapturingAllowGuardian {
        requests: Vec<crate::tool::PermissionRequest>,
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
                FixedGuardianOutcome::AskUser => Ok(PermissionGuardianDecision::AskUser {
                    rationale: "confirm the destination".to_string(),
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

    #[async_trait::async_trait(?Send)]
    impl PermissionGuardian for CapturingAllowGuardian {
        async fn review(
            &mut self,
            request: &crate::tool::PermissionRequest,
            _evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
        ) -> Result<PermissionGuardianDecision, PermissionGuardianError> {
            self.requests.push(request.clone());
            Ok(PermissionGuardianDecision::Allow {
                rationale: "scoped elevation".to_string(),
            })
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
                provider_connection: None,
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
            managed_shells: Default::default(),
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
    fn risk_free_shell_uses_workspace_sandbox_without_review_in_workspace_modes() {
        let request = permission(AccessKind::Shell, Vec::new());

        let decisions = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ]
        .map(|mode| access_mode_allows_permission(mode, &request));
        assert_eq!(decisions, [true, true, true]);
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
            (AccessKind::Shell, [true, true, true]),
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
                control.clone(),
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
                vec![crate::tool::PermissionRisk::ExternalConnection],
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
                    vec![crate::tool::PermissionRisk::ExternalConnection],
                )
                .await,
            Err(ToolError::PermissionDenied { .. })
        ));
        drop(context);
        assert_eq!(prompt.requests, 1);
    }

    #[tokio::test]
    async fn live_mode_switch_changes_the_next_plan_but_not_an_admitted_effect() {
        let (config, session, services) = permission_fixture(AccessMode::Default).await;
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

        let admitted = context
            .confirm_if_needed(
                AccessKind::Shell,
                "run inside sandbox".to_string(),
                Vec::new(),
                false,
                Vec::new(),
            )
            .await
            .expect("default safe shell admission");
        assert!(matches!(
            admitted.sandbox_plan(),
            ProcessSandboxPlan::WorkspaceWrite(_)
        ));

        services
            .store
            .session_repo()
            .compare_and_set_root_session_access_mode(
                session.session.id,
                AccessMode::Default,
                AccessMode::FullAccess,
            )
            .await
            .expect("live mode switch")
            .expect("root session owner");

        let next = context
            .confirm_if_needed(
                AccessKind::Shell,
                "run after switch".to_string(),
                Vec::new(),
                false,
                Vec::new(),
            )
            .await
            .expect("next full-access admission");
        assert!(matches!(
            next.sandbox_plan(),
            ProcessSandboxPlan::Unrestricted
        ));
        assert!(matches!(
            admitted.sandbox_plan(),
            ProcessSandboxPlan::WorkspaceWrite(_)
        ));
        drop(context);
        assert_eq!(prompt.requests, 0);
    }

    #[test]
    fn non_process_admission_does_not_compile_an_unused_process_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        let mut config = crate::config::ResolvedConfig::default();
        config.permissions.additional_write_roots = vec![root.join("missing-write-root")];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        assert_eq!(
            process_sandbox_plan_for_admission(
                AccessKind::Read,
                AccessMode::Default,
                &workspace,
                &config,
            )
            .expect("read does not need a process profile"),
            ProcessSandboxPlan::NoProcess
        );
        assert!(
            process_sandbox_plan_for_admission(
                AccessKind::Shell,
                AccessMode::Default,
                &workspace,
                &config,
            )
            .is_err(),
            "a shell admission must still fail closed when a writable root is invalid"
        );
    }

    #[test]
    fn configured_instruction_target_is_shared_permission_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        let mut config = crate::config::ResolvedConfig::default();
        config.instructions.additional_files = vec![Utf8PathBuf::from("policy.md")];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        assert!(targets_configured_instruction_authority(
            &config,
            &workspace,
            &root.join("policy.md")
        ));
        assert!(!targets_configured_instruction_authority(
            &config,
            &workspace,
            &root.join("other.md")
        ));
    }

    #[tokio::test]
    async fn auto_review_allow_and_human_handoff_require_an_owned_retry_lease() {
        for outcome in [
            FixedGuardianOutcome::Allow,
            FixedGuardianOutcome::AskUser,
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
                    vec![crate::tool::PermissionRisk::ExternalConnection],
                )
                .await;
            drop(context);
            match guardian.outcome {
                FixedGuardianOutcome::Allow
                | FixedGuardianOutcome::AskUser
                | FixedGuardianOutcome::Fail => {
                    assert!(matches!(result, Err(ToolError::Message(_))))
                }
                FixedGuardianOutcome::Deny => {
                    assert!(matches!(result, Err(ToolError::PermissionDenied { .. })))
                }
            }
            assert_eq!(guardian.requests, 1);
            assert_eq!(prompt.requests, 0);
        }
    }

    #[tokio::test]
    async fn auto_review_handoff_keeps_exact_claim_until_human_decision_and_rechecks_authority() {
        struct HandoffGuardian {
            lease: Option<crate::storage::PermissionReviewLease>,
            timeout: bool,
        }
        #[async_trait::async_trait(?Send)]
        impl PermissionGuardian for HandoffGuardian {
            async fn review(
                &mut self,
                _request: &crate::tool::PermissionRequest,
                _evidence: &crate::tool::permission_guardian::PermissionGuardianEvidence,
            ) -> Result<PermissionGuardianDecision, PermissionGuardianError> {
                if self.timeout {
                    Err(PermissionGuardianError::TotalDeadline {
                        milliseconds: 60_000,
                    })
                } else {
                    Ok(PermissionGuardianDecision::AskUser {
                        rationale: "接続先の確認が必要です".into(),
                    })
                }
            }
            fn take_retry_lease(&mut self) -> Option<crate::storage::PermissionReviewLease> {
                self.lease.take()
            }
        }
        struct HandoffPrompt {
            store: StoreBundle,
            lease: crate::storage::PermissionReviewLease,
            control: RunControl,
            action: &'static str,
            requests: usize,
        }
        impl ConfirmationPrompt for HandoffPrompt {
            fn confirm(
                &mut self,
                request: &crate::tool::PermissionRequest,
            ) -> Result<ReviewDecision, crate::error::CliPromptError> {
                self.requests += 1;
                let claim = self.lease.claim();
                let fence = self.store.permission_retry_fence_store();
                assert_eq!(
                    fence.record(&claim.key).unwrap().unwrap().state,
                    crate::storage::PermissionRetryFenceState::Reviewing
                );
                assert!(matches!(
                    fence
                        .begin_review(
                            claim.key.clone(),
                            claim.authority_history_item_id,
                            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        )
                        .unwrap(),
                    crate::storage::BeginPermissionReview::Blocked(_)
                ));
                assert_eq!(request.summary, "run exact command");
                assert_eq!(
                    request.targets,
                    vec![Utf8PathBuf::from("C:/workspace/target")]
                );
                assert!(
                    request
                        .details
                        .iter()
                        .any(|detail| detail.contains("代理承認からの確認"))
                );
                match self.action {
                    "denied" => Ok(ReviewDecision::Denied),
                    "abort" => Ok(ReviewDecision::Abort),
                    "stop" => {
                        self.control
                            .interrupt(crate::protocol::TurnInterruptionCause::UserStop);
                        Ok(ReviewDecision::Approved)
                    }
                    "authority_changed" => {
                        self.store
                            .protocol_event_store()
                            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                                id: crate::protocol::HistoryItemId::new(),
                                session_id: claim.key.root_session_id(),
                                scope: crate::protocol::HistoryScope::Turn {
                                    turn_id: TurnId::new(),
                                },
                                sequence_no: 1,
                                created_at_ms: 2,
                                payload: crate::protocol::HistoryItemPayload::UserTurn {
                                    content: vec![crate::protocol::ContentPart::Text {
                                        text: "stop the previous action".into(),
                                    }],
                                    prompt_dispatch: None,
                                    editor_context: None,
                                },
                            })
                            .unwrap();
                        Ok(ReviewDecision::Approved)
                    }
                    "lost_claim" => {
                        assert_eq!(
                            self.lease.release().unwrap(),
                            crate::storage::PermissionReviewTransition::Applied
                        );
                        Ok(ReviewDecision::Approved)
                    }
                    _ => Ok(ReviewDecision::Approved),
                }
            }
        }
        for action in [
            "approved",
            "denied",
            "abort",
            "stop",
            "authority_changed",
            "lost_claim",
            "timeout",
        ] {
            let (config, session, services) = permission_fixture(AccessMode::AutoReview).await;
            let authority_id = crate::protocol::HistoryItemId::new();
            services
                .store
                .protocol_event_store()
                .seed_history_item_for_test(&crate::protocol::HistoryItem {
                    id: authority_id,
                    session_id: session.session.id,
                    scope: crate::protocol::HistoryScope::Turn {
                        turn_id: TurnId::new(),
                    },
                    sequence_no: 0,
                    created_at_ms: 1,
                    payload: crate::protocol::HistoryItemPayload::UserTurn {
                        content: vec![crate::protocol::ContentPart::Text {
                            text: "perform the requested work".into(),
                        }],
                        prompt_dispatch: None,
                        editor_context: None,
                    },
                })
                .unwrap();
            let key = crate::storage::PermissionRetryFenceKey::new(
                session.session.id,
                1,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
            let lease = match services
                .store
                .permission_retry_fence_store()
                .begin_review(
                    key.clone(),
                    authority_id,
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .unwrap()
            {
                crate::storage::BeginPermissionReview::Claimed(lease) => lease,
                other => panic!("expected claim: {other:?}"),
            };
            let control = RunControl::new();
            let mut guardian = HandoffGuardian {
                lease: Some(lease.clone()),
                timeout: action == "timeout",
            };
            let mut prompt = HandoffPrompt {
                store: services.store.clone(),
                lease: lease.clone(),
                control: control.clone(),
                action,
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
                    control.clone(),
                ),
                prompt: &mut prompt,
                services: &services,
                agent: None,
                permission_guardian: Some(&mut guardian),
            };
            let result = context
                .confirm_if_needed(
                    AccessKind::Shell,
                    "run exact command".into(),
                    vec![Utf8PathBuf::from("C:/workspace/target")],
                    true,
                    vec![crate::tool::PermissionRisk::ExternalConnection],
                )
                .await;
            drop(context);
            assert_eq!(prompt.requests, 1, "{action}");
            match action {
                "approved" | "timeout" => {
                    let admission = result.as_ref().unwrap();
                    assert!(matches!(
                        admission.sandbox_plan(),
                        ProcessSandboxPlan::Unrestricted
                    ));
                    admission.admit().expect("human-approved exact admission");
                    assert!(!control.is_cancelled());
                }
                "authority_changed" => assert!(matches!(
                    result.as_ref().unwrap().admit(),
                    Err(ToolError::PermissionDenied { .. })
                )),
                "denied" => assert!(matches!(result, Err(ToolError::PermissionDenied { .. }))),
                "abort" => assert!(matches!(result, Err(ToolError::PermissionAborted))),
                "stop" => assert!(matches!(result, Err(ToolError::RunInterrupted))),
                "lost_claim" => assert!(matches!(result, Err(ToolError::Message(_)))),
                _ => unreachable!(),
            }
            drop(result);
            drop(prompt);
            drop(guardian);
            drop(lease);
            let stored = services
                .store
                .permission_retry_fence_store()
                .record(&key)
                .unwrap();
            if action == "denied" {
                assert_eq!(
                    stored.unwrap().state,
                    crate::storage::PermissionRetryFenceState::Denied
                );
            } else {
                assert!(
                    stored.is_none(),
                    "{action}: stale or settled claim must not survive"
                );
            }
            if matches!(action, "denied" | "abort" | "stop" | "lost_claim") {
                assert!(control.is_cancelled(), "{action}");
            }
        }
    }

    #[tokio::test]
    async fn guardian_evidence_discloses_the_requested_sandbox_elevation() {
        let (config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let control = RunControl::new();
        let mut prompt = CountingPrompt::default();
        let mut guardian = CapturingAllowGuardian::default();
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
            .confirm_if_needed_with_details(
                AccessKind::Shell,
                "run with explicitly requested elevation".to_string(),
                vec!["Requested sandbox elevation: needs an exact external effect".to_string()],
                Vec::new(),
                true,
                Vec::new(),
            )
            .await;
        assert!(
            matches!(result, Err(ToolError::Message(_))),
            "a Guardian Allow without the mandatory durable lease must fail closed"
        );
        drop(context);
        assert_eq!(prompt.requests, 0);
        assert_eq!(guardian.requests.len(), 1);
        assert!(guardian.requests[0].outside_workspace);
        assert!(guardian.requests[0].risks.is_empty());
        assert!(guardian.requests[0].details.iter().any(|detail| {
            detail == "Requested sandbox elevation: needs an exact external effect"
        }));
        assert!(
            guardian.requests[0].details.iter().any(|detail| {
                detail.contains("elevation outside the workspace-write OS sandbox")
            })
        );
    }

    #[tokio::test]
    async fn non_process_guardian_request_does_not_claim_process_elevation() {
        let (config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let control = RunControl::new();
        let mut prompt = CountingPrompt::default();
        let mut guardian = CapturingAllowGuardian::default();
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
                AccessKind::Read,
                "send a file to a configured service".to_string(),
                Vec::new(),
                false,
                vec![crate::tool::PermissionRisk::ConfiguredLocalService],
            )
            .await;
        assert!(
            matches!(result, Err(ToolError::Message(_))),
            "a non-process Allow also requires the same durable lease"
        );
        drop(context);
        assert_eq!(prompt.requests, 0);
        assert_eq!(guardian.requests.len(), 1);
        assert!(
            guardian.requests[0]
                .details
                .iter()
                .all(|detail| !detail.contains("workspace-write OS sandbox"))
        );
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
                control.clone(),
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
                vec![crate::tool::PermissionRisk::ExternalConnection],
                PermissionGuardianEvidenceState::incomplete(
                    "a sensitive executable field was redacted",
                ),
            )
            .await;
        drop(context);

        assert!(matches!(result, Err(ToolError::Message(_))));
        assert!(
            control.is_cancelled(),
            "an unfenced AutoReview admission error must terminate the run"
        );
        assert_eq!(guardian.requests, 0);
        assert_eq!(prompt.requests, 0);
    }

    #[tokio::test]
    async fn effect_admission_missing_owned_fence_fails_the_run_before_any_effect() {
        let (_config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let authority_id = crate::protocol::HistoryItemId::new();
        services
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: authority_id,
                session_id: session.session.id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: TurnId::new(),
                },
                sequence_no: 0,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "authorize one review".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("seed canonical authority");
        let key = crate::storage::PermissionRetryFenceKey::new(
            session.session.id,
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("retry key");
        let lease = match services
            .store
            .permission_retry_fence_store()
            .begin_review(
                key,
                authority_id,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .expect("begin review")
        {
            crate::storage::BeginPermissionReview::Claimed(lease) => lease,
            other => panic!("expected claimed review, got {other:?}"),
        };
        assert_eq!(
            lease.mark_allowed_pending().expect("record allow"),
            crate::storage::PermissionReviewTransition::Applied
        );
        let control = RunControl::new();
        let admission = ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::NoProcess)
            .with_permission_retry_lease(Some(lease.clone()));
        assert_eq!(
            lease.release().expect("delete owned fence fixture"),
            crate::storage::PermissionReviewTransition::Applied
        );

        assert!(matches!(admission.admit(), Err(ToolError::Message(_))));
        assert!(control.is_cancelled());
    }

    fn started_effect_review_lease(
        services: &ToolServices,
        session_id: SessionId,
    ) -> (
        crate::storage::PermissionRetryFenceKey,
        crate::protocol::HistoryItemId,
        crate::storage::PermissionReviewLease,
    ) {
        let authority_id = crate::protocol::HistoryItemId::new();
        services
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: authority_id,
                session_id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: TurnId::new(),
                },
                sequence_no: 0,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "start a managed process, then independently verify it".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("seed canonical authority");
        let key = crate::storage::PermissionRetryFenceKey::new(
            session_id,
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("retry key");
        let lease = match services
            .store
            .permission_retry_fence_store()
            .begin_review(
                key.clone(),
                authority_id,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .expect("begin review")
        {
            crate::storage::BeginPermissionReview::Claimed(lease) => lease,
            other => panic!("expected claimed review, got {other:?}"),
        };
        (key, authority_id, lease)
    }

    #[tokio::test]
    async fn started_effect_finish_releases_the_review_before_worker_ticket_is_dropped() {
        let (_config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let (key, authority_id, lease) = started_effect_review_lease(&services, session.session.id);
        assert_eq!(
            lease.mark_allowed_pending().expect("record allow"),
            crate::storage::PermissionReviewTransition::Applied
        );
        let control = RunControl::new();
        let admission = ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::Unrestricted)
            .with_permission_retry_lease(Some(lease));
        let worker_ticket = admission.clone();
        worker_ticket
            .admit()
            .expect("admit the exact startup effect");
        admission
            .finish_started_effect()
            .expect("settle successful startup");
        assert!(!control.is_cancelled());

        let store = services.store.permission_retry_fence_store();
        assert!(store.record(&key).expect("read released fence").is_none());
        let next_lease = match store
            .begin_review(
                key.clone(),
                authority_id,
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )
            .expect("review the independent follow-up under the same user instruction")
        {
            crate::storage::BeginPermissionReview::Claimed(lease) => lease,
            other => panic!("expected a new review after startup, got {other:?}"),
        };
        drop(worker_ticket);
        assert_eq!(
            store
                .record(&key)
                .expect("read subsequent review")
                .expect("worker cleanup must preserve the new review")
                .review_id,
            next_lease.claim().review_id
        );
        assert_eq!(
            next_lease.release().expect("release fixture review"),
            crate::storage::PermissionReviewTransition::Applied
        );
    }

    #[test]
    fn started_effect_finish_without_auto_review_does_not_cancel_the_run() {
        let control = RunControl::new();
        ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::Unrestricted)
            .finish_started_effect()
            .expect("startup with no automatic review needs no fence settlement");
        assert!(!control.is_cancelled());
    }

    #[tokio::test]
    async fn started_effect_finish_lost_ownership_fails_the_run() {
        let (_config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let (_key, _authority_id, lease) =
            started_effect_review_lease(&services, session.session.id);
        assert_eq!(
            lease.mark_allowed_pending().expect("record allow"),
            crate::storage::PermissionReviewTransition::Applied
        );
        let control = RunControl::new();
        let admission = ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::Unrestricted)
            .with_permission_retry_lease(Some(lease.clone()));
        admission.admit().expect("admit startup");
        assert_eq!(
            lease.release().expect("remove the owned fixture claim"),
            crate::storage::PermissionReviewTransition::Applied
        );

        assert!(matches!(
            admission.finish_started_effect(),
            Err(ToolError::Message(_))
        ));
        assert!(control.is_cancelled());
    }

    #[tokio::test]
    async fn started_effect_finish_storage_failure_fails_the_run_and_retains_the_fence() {
        let (_config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let (key, _authority_id, lease) =
            started_effect_review_lease(&services, session.session.id);
        assert_eq!(
            lease.mark_allowed_pending().expect("record allow"),
            crate::storage::PermissionReviewTransition::Applied
        );
        let control = RunControl::new();
        let admission = ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::Unrestricted)
            .with_permission_retry_lease(Some(lease.clone()));
        admission.admit().expect("admit startup");
        let database = rusqlite::Connection::open(&services.storage_paths.database_path)
            .expect("open this test's isolated database");
        database
            .execute_batch(
                "CREATE TRIGGER fixture_block_fence_release
                 BEFORE DELETE ON permission_retry_fences
                 BEGIN SELECT RAISE(ABORT, 'fixture release failure'); END;",
            )
            .expect("inject a durable-settlement failure");

        assert!(matches!(
            admission.finish_started_effect(),
            Err(ToolError::Message(_))
        ));
        assert!(control.is_cancelled());
        assert_eq!(
            services
                .store
                .permission_retry_fence_store()
                .record(&key)
                .expect("read retained fence")
                .expect("failed settlement must remain fenced")
                .state,
            crate::storage::PermissionRetryFenceState::Admitted
        );
        database
            .execute_batch("DROP TRIGGER fixture_block_fence_release;")
            .expect("remove fixture failure");
        assert_eq!(
            lease.release().expect("clean up owned fixture claim"),
            crate::storage::PermissionReviewTransition::Applied
        );
    }

    #[tokio::test]
    async fn started_effect_finish_cannot_remove_a_denied_claim() {
        let (_config, session, services) = permission_fixture(AccessMode::AutoReview).await;
        let (key, _authority_id, lease) =
            started_effect_review_lease(&services, session.session.id);
        assert_eq!(
            lease
                .mark_denied(crate::storage::PermissionRetryFenceOutcome::GuardianDenied)
                .expect("record Guardian denial"),
            crate::storage::PermissionReviewTransition::Applied
        );
        let control = RunControl::new();
        let admission = ToolEffectAdmission::new(control.clone(), ProcessSandboxPlan::Unrestricted)
            .with_permission_retry_lease(Some(lease));

        assert!(matches!(
            admission.finish_started_effect(),
            Err(ToolError::Message(_))
        ));
        assert!(control.is_cancelled());
        let denied = services
            .store
            .permission_retry_fence_store()
            .record(&key)
            .expect("read rejection fence")
            .expect("settlement must not remove a rejected operation");
        assert_eq!(
            denied.state,
            crate::storage::PermissionRetryFenceState::Denied
        );
        assert_eq!(
            denied.outcome,
            Some(crate::storage::PermissionRetryFenceOutcome::GuardianDenied)
        );
    }

    #[tokio::test]
    async fn remote_wait_fence_reads_cross_store_steer_without_an_agent_or_consuming_input() {
        use crate::protocol::{SteerTurn, UserInputItem, UserTurn};

        let (store, session_id) = fence_test_session().await;
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admitted = repo
            .admit_session_turn(session_id, turn_id)
            .await
            .unwrap()
            .unwrap();
        let admission_id = admitted.admission_id;
        repo.append_user_turn_with_protocol_bundle(
            session_id,
            admission_id,
            &UserTurn {
                turn_id,
                items: vec![UserInputItem::Text {
                    text: "Wait for the remote task".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
            turn_id,
            0,
        )
        .await
        .unwrap();
        let fence = RunMutationFence::new(
            repo.clone(),
            session_id,
            admission_id,
            turn_id,
            RunControl::new(),
        );
        assert_eq!(
            fence.origin_turn_revision().await.unwrap(),
            admitted.admission_revision
        );
        assert!(!fence.has_pending_turn_steer_input().unwrap());
        let other_store = StoreBundle::new(SqliteStore::open(store.paths()).unwrap());
        let steer_id = other_store
            .session_repo()
            .accept_active_turn_steer(
                session_id,
                1,
                &SteerTurn {
                    expected_turn_id: turn_id,
                    items: vec![UserInputItem::Text {
                        text: "Inspect the latest result first".into(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: None,
                },
            )
            .await
            .unwrap();
        assert!(fence.has_pending_turn_steer_input().unwrap());
        assert!(fence.has_pending_turn_steer_input().unwrap());
        let stale = RunMutationFence::new(
            repo.clone(),
            session_id,
            admission_id,
            TurnId::new(),
            RunControl::new(),
        );
        assert!(stale.has_pending_turn_steer_input().is_err());
        assert_eq!(
            repo.deliver_all_pending_turn_steers_for_admitted_turn(
                session_id,
                admission_id,
                turn_id
            )
            .unwrap(),
            vec![steer_id]
        );
        assert!(!fence.has_pending_turn_steer_input().unwrap());
    }

    #[tokio::test]
    async fn run_mutation_fence_rejects_cancelled_expired_and_tree_stopped_owners() {
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

        let (stopped_store, stopped_session_id) = fence_test_session().await;
        let stopped_repo = stopped_store.session_repo();
        let stopped_turn_id = TurnId::new();
        let stopped_admission_id = stopped_repo
            .admit_session_turn(stopped_session_id, stopped_turn_id)
            .await
            .expect("tree-stop admission")
            .expect("tree-stop admitted")
            .admission_id;
        let stopped_control = RunControl::new();
        let stopped_fence = RunMutationFence::new(
            stopped_repo.clone(),
            stopped_session_id,
            stopped_admission_id,
            stopped_turn_id,
            stopped_control.clone(),
        );
        stopped_fence
            .assert_owned()
            .await
            .expect("owner before durable tree Stop");
        stopped_repo
            .record_agent_tree_stop_fence(
                stopped_session_id,
                crate::protocol::TurnInterruptionCause::UserStop,
            )
            .await
            .expect("durable tree Stop")
            .expect("tree Stop fence");
        assert!(
            stopped_fence.assert_owned().await.is_err(),
            "a durable tree Stop must revoke workspace mutation ownership before fanout"
        );
        assert!(stopped_control.is_cancelled());
    }
}
