use crate::session::SessionId;

#[derive(Debug, Clone, Default)]
pub struct DesktopComposerState {
    pub draft_prompt: String,
    pub image_attachment_input: String,
    pub image_attachment_paths: Vec<camino::Utf8PathBuf>,
    pub review_draft_text: String,
    owner_workspace_path: String,
    owner_session_id: Option<SessionId>,
}

impl DesktopComposerState {
    pub fn for_owner(workspace_path: String, session_id: Option<SessionId>) -> Self {
        Self {
            owner_workspace_path: workspace_path,
            owner_session_id: session_id,
            ..Self::default()
        }
    }

    pub fn rebind_owner(&mut self, workspace_path: &str, session_id: Option<SessionId>) -> bool {
        if self.owner_workspace_path == workspace_path && self.owner_session_id == session_id {
            return false;
        }
        self.clear_request_inputs();
        self.owner_workspace_path = workspace_path.to_string();
        self.owner_session_id = session_id;
        true
    }

    pub fn adopt_owner(&mut self, workspace_path: &str, session_id: Option<SessionId>) {
        self.owner_workspace_path = workspace_path.to_string();
        self.owner_session_id = session_id;
    }

    pub fn is_owned_by(&self, workspace_path: &str, session_id: Option<SessionId>) -> bool {
        self.owner_workspace_path == workspace_path && self.owner_session_id == session_id
    }

    pub fn clear_request_inputs(&mut self) {
        self.draft_prompt.clear();
        self.image_attachment_input.clear();
        self.image_attachment_paths.clear();
        self.review_draft_text.clear();
    }
}
