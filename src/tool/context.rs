use std::sync::Arc;

use camino::Utf8PathBuf;
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::{AccessMode, ResolvedConfig};
use crate::edit::{ChangeTracker, EditSafety, Formatter};
use crate::error::ToolError;
use crate::protocol::TurnId;
use crate::runtime::LiveConfigOverrides;
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
    pub run_mutation_fence: RunMutationFence,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
    pub agent: Option<&'a crate::app::AgentRunContext>,
}

#[derive(Clone)]
pub struct RunMutationFence {
    repo: SqliteSessionRepository,
    session_id: SessionId,
    admission_id: String,
    turn_id: TurnId,
    cancel: CancellationToken,
}

impl RunMutationFence {
    pub fn new(
        repo: SqliteSessionRepository,
        session_id: SessionId,
        admission_id: String,
        turn_id: TurnId,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            repo,
            session_id,
            admission_id,
            turn_id,
            cancel,
        }
    }

    pub async fn assert_owned(&self) -> Result<(), ToolError> {
        if self.cancel.is_cancelled() {
            return Err(self.rejected_error("the run is cancelled"));
        }
        let outcome = match self
            .repo
            .renew_admitted_run_lease(self.session_id, &self.admission_id, self.turn_id)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.cancel.cancel();
                return Err(ToolError::Storage(error));
            }
        };
        if outcome != RunAdmissionLeaseRenewalOutcome::Renewed {
            self.cancel.cancel();
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
        if self.cancel.is_cancelled() {
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
}

impl<'a> ToolContext<'a> {
    pub fn confirm_if_needed(
        &mut self,
        access: AccessKind,
        summary: String,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<crate::tool::PermissionRisk>,
    ) -> Result<(), ToolError> {
        self.confirm_if_needed_with_details(
            access,
            summary,
            Vec::new(),
            targets,
            outside_workspace,
            risks,
        )
    }

    pub fn confirm_if_needed_with_details(
        &mut self,
        access: AccessKind,
        summary: String,
        details: Vec<String>,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<crate::tool::PermissionRisk>,
    ) -> Result<(), ToolError> {
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

        if access_mode_allows_permission(self.current_access_mode(), &request) {
            return Ok(());
        }

        if self
            .prompt
            .confirm_with_cancel(&request, &self.cancel)
            .map_err(|error| {
                ToolError::Message(format!("failed to prompt for permission: {error}"))
            })?
        {
            Ok(())
        } else {
            Err(ToolError::Message("permission denied by user".to_string()))
        }
    }

    fn current_access_mode(&self) -> AccessMode {
        self.live_config
            .as_ref()
            .map(LiveConfigOverrides::access_mode)
            .unwrap_or(self.config.permissions.access_mode)
    }
}

pub fn access_mode_allows_permission(
    access_mode: AccessMode,
    request: &crate::tool::PermissionRequest,
) -> bool {
    if request
        .risks
        .iter()
        .any(|risk| matches!(risk, crate::tool::PermissionRisk::ExternalConnection))
    {
        return false;
    }
    match access_mode {
        AccessMode::FullAccess => full_access_allows(request),
        AccessMode::AutoReview => auto_review_allows(request),
        AccessMode::Default => default_allows(request),
    }
}

fn full_access_allows(request: &crate::tool::PermissionRequest) -> bool {
    if request.outside_workspace {
        return false;
    }
    !request.risks.iter().any(|risk| {
        matches!(
            risk,
            crate::tool::PermissionRisk::Network
                | crate::tool::PermissionRisk::ExternalConnection
                | crate::tool::PermissionRisk::ProtectedWorkspaceAuthority
        )
    })
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
    if request.outside_workspace || !request.risks.is_empty() {
        return false;
    }
    match request.access {
        AccessKind::List | AccessKind::Search | AccessKind::Read => true,
        AccessKind::Edit => true,
        AccessKind::Shell => false,
    }
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
        assert!(!access_mode_allows_permission(
            AccessMode::AutoReview,
            &request
        ));
        assert!(access_mode_allows_permission(
            AccessMode::FullAccess,
            &request
        ));
    }

    #[test]
    fn access_mode_policy_still_blocks_external_connections() {
        let request = permission(
            AccessKind::Shell,
            vec![crate::tool::PermissionRisk::ExternalConnection],
        );

        assert!(!access_mode_allows_permission(
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
    fn every_access_mode_reviews_boundary_and_hard_risk_requests() {
        let modes = [
            AccessMode::Default,
            AccessMode::AutoReview,
            AccessMode::FullAccess,
        ];
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
            let mut outside = permission(AccessKind::Read, Vec::new());
            outside.outside_workspace = true;
            assert!(!access_mode_allows_permission(mode, &outside));
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
    async fn run_mutation_fence_rejects_cancelled_and_expired_owners_before_mutation() {
        let (store, session_id) = fence_test_session().await;
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_run(session_id)
            .await
            .expect("admission")
            .expect("admitted");
        let turn_id = TurnId::new();
        assert!(
            repo.activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        let cancel = CancellationToken::new();
        let fence = RunMutationFence::new(repo, session_id, admission_id, turn_id, cancel.clone());
        fence.assert_owned().await.expect("fresh owner");
        cancel.cancel();
        let mut cancelled_mutation_ran = false;
        if fence.assert_owned().await.is_ok() {
            cancelled_mutation_ran = true;
        }
        assert!(!cancelled_mutation_ran);

        let (expired_store, expired_session_id) = fence_test_session().await;
        let expired_repo = expired_store.session_repo();
        let expired_admission_id = expired_repo
            .admit_session_run_at(expired_session_id, 0, 1)
            .await
            .expect("expired admission")
            .expect("admitted");
        let expired_cancel = CancellationToken::new();
        let expired_fence = RunMutationFence::new(
            expired_repo,
            expired_session_id,
            expired_admission_id,
            TurnId::new(),
            expired_cancel.clone(),
        );
        let mut expired_mutation_ran = false;
        if expired_fence.assert_owned().await.is_ok() {
            expired_mutation_ran = true;
        }
        assert!(!expired_mutation_ran);
        assert!(expired_cancel.is_cancelled());
    }
}
