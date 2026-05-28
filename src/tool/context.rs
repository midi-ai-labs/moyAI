use std::sync::Arc;

use camino::Utf8PathBuf;
use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::{AccessMode, ResolvedConfig};
use crate::edit::{ChangeTracker, EditSafety, Formatter};
use crate::error::ToolError;
use crate::session::{SessionContext, ToolCallId};
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
}

pub struct ToolContext<'a> {
    pub session: &'a SessionContext,
    pub workspace: &'a Workspace,
    pub config: &'a ResolvedConfig,
    pub tool_call_id: ToolCallId,
    pub cancel: CancellationToken,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
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
        };

        if permission_preset_allows(self.config.permissions.access_mode, &request) {
            return Ok(());
        }

        if self.prompt.confirm(&request).map_err(|error| {
            ToolError::Message(format!("failed to prompt for permission: {error}"))
        })? {
            Ok(())
        } else {
            Err(ToolError::Message("permission denied by user".to_string()))
        }
    }
}

fn permission_preset_allows(
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
        AccessMode::FullAccess => true,
        AccessMode::AutoReview => auto_review_allows(request),
        AccessMode::Default => default_allows(request),
    }
}

fn default_allows(request: &crate::tool::PermissionRequest) -> bool {
    !request.outside_workspace && request.risks.is_empty()
}

fn auto_review_allows(request: &crate::tool::PermissionRequest) -> bool {
    if request.outside_workspace {
        return false;
    }
    match request.access {
        AccessKind::List | AccessKind::Search | AccessKind::Read => true,
        AccessKind::Edit => !request
            .risks
            .iter()
            .any(PermissionRiskClass::requires_review),
        AccessKind::Shell => false,
    }
}

trait PermissionRiskClass {
    fn requires_review(&self) -> bool;
}

impl PermissionRiskClass for crate::tool::PermissionRisk {
    fn requires_review(&self) -> bool {
        true
    }
}
