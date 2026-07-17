use crate::session::SessionId;

#[derive(Debug, Clone, Default)]
pub struct DesktopComposerState {
    pub draft_prompt: String,
    pub image_attachment_input: String,
    pub image_attachment_paths: Vec<camino::Utf8PathBuf>,
    pub review_draft_text: String,
    owner_workspace_path: String,
    owner_session_id: Option<SessionId>,
    owner_generation: u64,
}

impl DesktopComposerState {
    pub fn for_owner(workspace_path: String, session_id: Option<SessionId>) -> Self {
        Self {
            owner_workspace_path: workspace_path,
            owner_session_id: session_id,
            owner_generation: 1,
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
        self.advance_owner_generation();
        true
    }

    pub fn reset_owner(&mut self, workspace_path: &str, session_id: Option<SessionId>) {
        self.clear_request_inputs();
        self.owner_workspace_path = workspace_path.to_string();
        self.owner_session_id = session_id;
        self.advance_owner_generation();
    }

    pub fn adopt_owner(&mut self, workspace_path: &str, session_id: Option<SessionId>) {
        self.owner_workspace_path = workspace_path.to_string();
        self.owner_session_id = session_id;
    }

    pub fn is_owned_by(&self, workspace_path: &str, session_id: Option<SessionId>) -> bool {
        self.owner_workspace_path == workspace_path && self.owner_session_id == session_id
    }

    pub fn owner_generation(&self) -> u64 {
        self.owner_generation
    }

    pub fn clear_request_inputs(&mut self) {
        self.draft_prompt.clear();
        self.image_attachment_input.clear();
        self.image_attachment_paths.clear();
        self.review_draft_text.clear();
    }

    fn advance_owner_generation(&mut self) {
        self.owner_generation = self.owner_generation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_owner_reset_clears_inputs_and_advances_generation() {
        let mut composer = DesktopComposerState::for_owner("C:/workspace".to_string(), None);
        composer.draft_prompt = "stale".to_string();
        composer.image_attachment_paths.push("C:/image.png".into());
        let before = composer.owner_generation();

        composer.reset_owner("C:/workspace", None);

        assert!(composer.draft_prompt.is_empty());
        assert!(composer.image_attachment_paths.is_empty());
        assert!(composer.owner_generation() > before);
    }

    #[test]
    fn adopting_a_created_session_keeps_the_logical_owner_generation() {
        let session_id = SessionId::new();
        let mut composer = DesktopComposerState::for_owner("C:/workspace".to_string(), None);
        let before = composer.owner_generation();

        composer.adopt_owner("C:/workspace", Some(session_id));

        assert_eq!(composer.owner_generation(), before);
    }
}
