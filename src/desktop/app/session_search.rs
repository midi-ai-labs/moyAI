use super::*;

impl DesktopController {
    pub(super) fn invalidate_session_search_requests(&mut self) {
        for operation_id in self.session_search_requests.clear() {
            let _ = self.state.finish_session_search(operation_id);
        }
    }

    pub(crate) fn set_session_search(&mut self, text: String) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state.set_session_search_text(text.clone());
        self.admit_session_search(text, self.state.view.session_search_include_archived);
        true
    }

    pub(crate) fn set_session_search_include_archived(&mut self, include_archived: bool) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state
            .set_session_search_include_archived(include_archived);
        self.admit_session_search(
            self.state.view.session_search_text.clone(),
            include_archived,
        );
        true
    }

    fn admit_session_search(&mut self, query: String, include_archived: bool) {
        let operation_id = self.state.begin_session_search();
        let target = SessionSearchRequestTarget {
            query,
            include_archived,
            selected_session_id: self.state.selected_session_id(),
        };
        let admission = self.session_search_requests.begin(operation_id, target);
        if let Some(operation_id) = admission.superseded_operation_id {
            let _ = self.state.finish_session_search(operation_id);
        }
        if let Some(dispatch) = admission.dispatch {
            self.spawn_session_search(dispatch);
        }
    }

    pub(super) fn spawn_session_search(
        &self,
        dispatch: SessionSearchDispatch<SessionSearchRequestTarget>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let SessionSearchDispatch { request_id, target } = dispatch;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-search runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_session_search(
                    &app,
                    &target.query,
                    target.include_archived,
                    target.selected_session_id,
                )
                .await
                .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionSearchLoaded { request_id, result });
        });
    }
}
