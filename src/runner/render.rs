use crate::cli::EventRenderer;
use crate::error::CliRenderError;
use crate::session::*;

pub(super) struct RunnerRenderer {
    pub host: super::RunnerHost,
    pub id: ulid::Ulid,
}

impl EventRenderer for RunnerRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        if let RunEvent::SessionStarted { session_id, .. } = event {
            if let Ok(mut state) = self.host.inner.state.lock() {
                if let Some(run) = state.runs.get_mut(&self.id) {
                    run.session_id = Some(*session_id);
                }
            }
        }
        if let RunEvent::AssistantMessageCommitted { response_id, text } = event {
            let mut length = text.len().min(32 * 1024);
            while !text.is_char_boundary(length) {
                length -= 1;
            }
            if let Ok(mut state) = self.host.inner.state.lock() {
                if let Some(run) = state.runs.get_mut(&self.id) {
                    run.response = Some((
                        *response_id,
                        text[..length].to_string(),
                        length < text.len(),
                    ));
                }
            }
        }
        // RunService already persists canonical events. A disconnected UI never loses history.
        Ok(())
    }
    fn finish(&mut self, _: &RunSummary) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_list(&mut self, _: &[SessionRecord]) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_loaded_sessions(&mut self, _: &LoadedSessionList) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_items(
        &mut self,
        _: &SessionRecord,
        _: &[crate::protocol::HistoryItem],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_page(
        &mut self,
        _: &CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_read(&mut self, _: &CanonicalSessionRead) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_rejoin(&mut self, _: &RunningSessionRejoin) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_turn_page(&mut self, _: &CanonicalTurnPage) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_runtime_event_page(
        &mut self,
        _: &CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}
