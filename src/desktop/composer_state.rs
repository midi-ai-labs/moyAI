#[derive(Debug, Clone, Default)]
pub struct DesktopComposerState {
    pub draft_prompt: String,
    pub image_attachment_input: String,
    pub image_attachment_paths: Vec<camino::Utf8PathBuf>,
    pub review_draft_text: String,
}

impl DesktopComposerState {
    pub fn clear_request_inputs(&mut self) {
        self.draft_prompt.clear();
        self.image_attachment_input.clear();
        self.image_attachment_paths.clear();
        self.review_draft_text.clear();
    }
}
