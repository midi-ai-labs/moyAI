//! Shared read execution with either a canonical agent owner or an explicit workspace.
//! Published reads have no session, edit baseline, internal-output or mutation authority.

use camino::{Utf8Path, Utf8PathBuf};

use crate::config::ResolvedConfig;
use crate::edit::EditSafety;
use crate::error::ToolError;
use crate::runtime::RunControl;
use crate::session::SessionId;
use crate::tool::PermissionRisk;
use crate::tool::context::{ToolContext, ToolEffectAdmission};
use crate::tool::internal_output::ResolvedPath;
use crate::tool::os_sandbox::ProcessSandboxPlan;
use crate::workspace::{AccessKind, PathGuard, Workspace};

pub struct ReadToolContext<'a> {
    owner: ReadOwner<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::registry::ToolRegistry;
    use crate::workspace::WorkspaceDiscovery;

    #[tokio::test]
    async fn published_context_rejects_additional_roots_mutation_and_cancelled_admission() {
        let directory = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(directory.path().to_owned()).unwrap();
        let selected = root.join("selected");
        let additional = root.join("additional");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&additional).unwrap();
        let mut config = ResolvedConfig::default();
        config
            .permissions
            .additional_read_roots
            .push(additional.clone());
        let workspace = WorkspaceDiscovery::discover_fixed_root(&selected, &config).unwrap();
        let control = RunControl::new();
        let mut context = ReadToolContext::published(&workspace, &config, control.clone());
        assert!(context.edit_baseline_owner().is_none());
        assert!(PathGuard::require_path(&workspace, &additional, AccessKind::Read).is_ok());
        assert!(
            context
                .resolve_path(&additional, AccessKind::Read)
                .await
                .is_err()
        );
        assert!(
            context
                .confirm_if_needed(
                    AccessKind::Read,
                    "read".into(),
                    vec![additional],
                    false,
                    vec![]
                )
                .await
                .is_err()
        );
        for access in [AccessKind::Edit, AccessKind::Shell] {
            assert!(
                context
                    .confirm_if_needed(
                        access,
                        "effect".into(),
                        vec![selected.clone()],
                        false,
                        vec![]
                    )
                    .await
                    .is_err()
            );
        }
        let admitted = context
            .confirm_if_needed(
                AccessKind::Read,
                "read".into(),
                vec![selected.clone()],
                false,
                vec![],
            )
            .await
            .unwrap();
        assert!(admitted.admit().is_ok());
        assert!(control.interrupt(crate::protocol::TurnInterruptionCause::UserStop));
        let cancelled = context
            .confirm_if_needed(
                AccessKind::Read,
                "read".into(),
                vec![selected],
                false,
                vec![],
            )
            .await
            .unwrap();
        assert!(cancelled.admit().is_err());
    }

    #[tokio::test]
    async fn static_read_classification_does_not_grant_session_bookkeeping_to_published_context() {
        let directory = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(directory.path().to_owned()).unwrap();
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).unwrap();
        let registry = ToolRegistry::core_agent();
        assert!(
            registry
                .execute_published_read(
                    "get_goal",
                    serde_json::json!({}),
                    ReadToolContext::published(&workspace, &config, RunControl::new())
                )
                .await
                .is_err()
        );
        assert!(
            registry
                .execute_published_read(
                    "write",
                    serde_json::json!({"path":"unexpected","content":"never"}),
                    ReadToolContext::published(&workspace, &config, RunControl::new())
                )
                .await
                .is_err()
        );
        assert!(!root.join("unexpected").exists());
    }
}

enum ReadOwner<'a> {
    Agent(ToolContext<'a>),
    Published {
        workspace: &'a Workspace,
        config: &'a ResolvedConfig,
        control: RunControl,
    },
}

impl<'a> ReadToolContext<'a> {
    pub(crate) fn agent(context: ToolContext<'a>) -> Self {
        Self {
            owner: ReadOwner::Agent(context),
        }
    }

    pub(crate) fn published(
        workspace: &'a Workspace,
        config: &'a ResolvedConfig,
        control: RunControl,
    ) -> Self {
        Self {
            owner: ReadOwner::Published {
                workspace,
                config,
                control,
            },
        }
    }

    pub(crate) fn workspace(&self) -> &Workspace {
        match &self.owner {
            ReadOwner::Agent(context) => context.workspace,
            ReadOwner::Published { workspace, .. } => workspace,
        }
    }

    pub(crate) fn config(&self) -> &ResolvedConfig {
        match &self.owner {
            ReadOwner::Agent(context) => context.config,
            ReadOwner::Published { config, .. } => config,
        }
    }

    pub(crate) fn edit_baseline_owner(&self) -> Option<(&EditSafety, SessionId)> {
        match &self.owner {
            ReadOwner::Agent(context) => {
                Some((&context.services.edit_safety, context.session.session.id))
            }
            ReadOwner::Published { .. } => None,
        }
    }

    pub(crate) async fn resolve_path(
        &self,
        requested: &Utf8Path,
        access: AccessKind,
    ) -> Result<ResolvedPath, ToolError> {
        match &self.owner {
            ReadOwner::Agent(context) => {
                crate::tool::internal_output::resolve_path(context, requested, access).await
            }
            ReadOwner::Published { workspace, .. } => {
                let guarded = PathGuard::require_path(workspace, requested, access)?;
                if !guarded.inside_workspace {
                    return Err(ToolError::Message(
                        "Published reads require the selected workspace".into(),
                    ));
                }
                Ok(ResolvedPath::workspace(guarded))
            }
        }
    }

    pub(crate) async fn confirm_if_needed(
        &mut self,
        access: AccessKind,
        summary: String,
        targets: Vec<Utf8PathBuf>,
        outside_workspace: bool,
        risks: Vec<PermissionRisk>,
    ) -> Result<ToolEffectAdmission, ToolError> {
        match &mut self.owner {
            ReadOwner::Agent(context) => {
                context
                    .confirm_if_needed(access, summary, targets, outside_workspace, risks)
                    .await
            }
            ReadOwner::Published {
                workspace, control, ..
            } => {
                if !matches!(
                    access,
                    AccessKind::Read | AccessKind::Search | AccessKind::List
                ) || outside_workspace
                    || !risks.is_empty()
                    || targets.is_empty()
                {
                    return Err(ToolError::Message(
                        "Published read authority does not allow this operation".into(),
                    ));
                }
                for target in targets {
                    if !PathGuard::require_path(workspace, &target, access)?.inside_workspace {
                        return Err(ToolError::Message(
                            "Published reads require the selected workspace".into(),
                        ));
                    }
                }
                Ok(ToolEffectAdmission::new(
                    control.clone(),
                    ProcessSandboxPlan::NoProcess,
                ))
            }
        }
    }
}
