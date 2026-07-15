use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use tokio_util::sync::CancellationToken;

use crate::cli::{ConfirmationOutcome, ConfirmationPrompt};
use crate::config::{AccessMode, ResolvedConfig};
use crate::edit::{ChangeTracker, EditSafety, Formatter, FormatterExecutionOptions};
use crate::error::ToolError;
use crate::llm::{LlmClient, ModelProfile};
use crate::protocol::{ToolApprovalDecision, TurnId};
use crate::runtime::{LiveConfigOverrides, RunControl};
use crate::runtime::{RunCancelOutcome, RunCancellationCause};
use crate::session::{SessionContext, SessionId, ToolCallId};
use crate::storage::{SqliteSessionRepository, session_repo::RunAdmissionLeaseRenewalOutcome};
use crate::storage::{StoragePaths, StoreBundle};
use crate::tool::truncate::ToolTruncator;
use crate::workspace::{AccessKind, Workspace};

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
    pub live_config: Option<LiveConfigOverrides>,
    pub tool_call_id: ToolCallId,
    pub cancel: CancellationToken,
    pub run_control: RunControl,
    pub run_mutation_fence: RunMutationFence,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
    pub agent: Option<&'a crate::app::AgentRunContext>,
    pub permission_reviewer_llm: &'a dyn LlmClient,
    pub permission_reviewer_model: &'a ModelProfile,
    pub permission_review_context: &'a str,
    pub model_request_gate: Option<Arc<tokio::sync::Semaphore>>,
    pub model_request_count: &'a mut usize,
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

    pub async fn format_if_configured(
        &self,
        formatter: &Formatter,
        path: &Utf8Path,
        normalized: String,
        options: FormatterExecutionOptions,
    ) -> Result<String, ToolError> {
        self.admit()?;
        formatter
            .format_if_configured(path, normalized, options)
            .await
            .map_err(ToolError::from)
    }
}

#[derive(Clone)]
pub struct RunMutationFence {
    repo: SqliteSessionRepository,
    session_id: SessionId,
    admission_id: String,
    turn_id: TurnId,
    control: RunControl,
}

impl RunMutationFence {
    pub fn new(
        repo: SqliteSessionRepository,
        session_id: SessionId,
        admission_id: String,
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
            .renew_admitted_run_lease(self.session_id, &self.admission_id, self.turn_id)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.control.fail(error.to_string());
                return Err(ToolError::Storage(error));
            }
        };
        if outcome != RunAdmissionLeaseRenewalOutcome::Renewed {
            match outcome {
                RunAdmissionLeaseRenewalOutcome::GracefulTerminal => {
                    self.control.supersede();
                }
                RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
                    self.control.supersede();
                }
                RunAdmissionLeaseRenewalOutcome::Renewed => unreachable!(),
            }
            return Err(self.rejected_error(match outcome {
                RunAdmissionLeaseRenewalOutcome::GracefulTerminal => {
                    "the admitted turn is already terminal"
                }
                RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
                    "the admission was superseded or its lease expired"
                }
                RunAdmissionLeaseRenewalOutcome::Renewed => unreachable!(),
            }));
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

        let access_mode = self.current_access_mode();
        if access_mode_allows_permission(access_mode, &request) {
            return self.accept_tool_effect();
        }

        if access_mode == AccessMode::AutoReview {
            *self.model_request_count += 1;
            let reviewer = crate::tool::permission_review::PermissionReviewer {
                llm: self.permission_reviewer_llm,
                model: self.permission_reviewer_model,
                config: self.config,
                request_gate: self.model_request_gate.clone(),
            };
            match reviewer
                .review(
                    self.permission_review_context,
                    &request,
                    self.cancel.clone(),
                )
                .await
            {
                Ok(decision) if decision.allows() => return self.accept_tool_effect(),
                Ok(decision) => request.details.push(format!(
                    "AI reviewer denied this request (risk: {}): {}",
                    decision.risk_level.label(),
                    decision.rationale
                )),
                Err(error) => request.details.push(format!(
                    "AI reviewer could not decide; human confirmation is required: {error}"
                )),
            }
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
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { .. }) => {
                let settlement = self
                    .run_control
                    .begin_tool_settlement()
                    .ok_or(ToolError::RunInterrupted)?;
                Err(ToolError::PermissionDenied {
                    settlement: Some(settlement),
                })
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

    fn current_access_mode(&self) -> AccessMode {
        self.live_config
            .as_ref()
            .map(LiveConfigOverrides::access_mode)
            .unwrap_or(self.config.permissions.access_mode)
    }

    fn accept_tool_effect(&self) -> Result<ToolEffectAdmission, ToolError> {
        Ok(ToolEffectAdmission::new(self.run_control.clone()))
    }
}

pub fn access_mode_allows_permission(
    access_mode: AccessMode,
    request: &crate::tool::PermissionRequest,
) -> bool {
    match access_mode {
        AccessMode::FullAccess => full_access_allows(request),
        AccessMode::AutoReview => auto_review_allows(request),
        AccessMode::Default => default_allows(request),
    }
}

fn full_access_allows(request: &crate::tool::PermissionRequest) -> bool {
    !request.outside_workspace
}

fn default_allows(request: &crate::tool::PermissionRequest) -> bool {
    if request.outside_workspace || !request.risks.is_empty() {
        return false;
    }
    matches!(
        request.access,
        AccessKind::List | AccessKind::Search | AccessKind::Read
    )
}

fn auto_review_allows(request: &crate::tool::PermissionRequest) -> bool {
    if request.outside_workspace
        || request
            .risks
            .iter()
            .any(|risk| !matches!(risk, crate::tool::PermissionRisk::ConfiguredLocalService))
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::session::{NewSession, ProjectId, ProjectRepository, SessionRepository};
    use crate::storage::{SqliteStore, StoragePaths};
    use crate::workspace::AccessKind;

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
    fn access_mode_policy_allows_shell_when_switched_to_full_access() {
        let request = permission(AccessKind::Shell, Vec::new());

        assert!(!access_mode_allows_permission(
            AccessMode::Default,
            &request
        ));
        assert!(access_mode_allows_permission(
            AccessMode::AutoReview,
            &request
        ));
        assert!(access_mode_allows_permission(
            AccessMode::FullAccess,
            &request
        ));
    }

    #[test]
    fn full_access_allows_detected_risks_inside_the_configured_boundary() {
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
    fn access_mode_policy_is_monotonic_for_risk_free_workspace_operations() {
        let cases = [
            (AccessKind::List, [true, true, true]),
            (AccessKind::Search, [true, true, true]),
            (AccessKind::Read, [true, true, true]),
            (AccessKind::Edit, [false, true, true]),
            (AccessKind::Shell, [false, true, true]),
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
    fn default_and_auto_review_keep_hard_risk_requests_for_review() {
        let modes = [AccessMode::Default, AccessMode::AutoReview];
        let hard_risks = [
            crate::tool::PermissionRisk::Network,
            crate::tool::PermissionRisk::ExternalConnection,
            crate::tool::PermissionRisk::ProtectedWorkspaceAuthority,
        ];

        for mode in modes {
            for risk in hard_risks {
                let request = permission(AccessKind::Shell, vec![risk]);
                assert!(!access_mode_allows_permission(mode, &request));
            }
        }
        let mut outside = permission(AccessKind::Read, Vec::new());
        outside.outside_workspace = true;
        for mode in [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ] {
            assert!(!access_mode_allows_permission(mode, &outside));
        }
    }

    #[test]
    fn configured_local_service_is_auto_allowed_after_default() {
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
        assert_eq!(decisions, [false, true, true]);
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
    async fn run_mutation_fence_rejects_cancelled_and_expired_owners_before_mutation() {
        let (store, session_id) = fence_test_session().await;
        let repo = store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repo
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admission")
            .expect("admitted");
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
            .expect("admitted");
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
