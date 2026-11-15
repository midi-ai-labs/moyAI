use super::*;

impl DesktopController {
    pub(crate) fn export_history_markdown_auto(&mut self, session_id: SessionId) {
        let Some(title) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
            .map(|row| row.label.clone())
        else {
            self.state
                .set_status_message("history export target is no longer available");
            return;
        };
        let default_file_name = history_markdown_file_name(&title, session_id);
        let export_path = self
            .app
            .workspace
            .authority_root()
            .join(".moyai")
            .join("history-exports")
            .join(default_file_name);
        self.export_history_markdown_to_path(session_id, export_path);
    }

    pub(crate) fn export_open_transcript_markdown_auto(&mut self) {
        if !self.state.can_export_history() {
            self.state.set_status_message(
                "transcript export cannot start while another operation is active",
            );
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting transcript");
            return;
        };
        let Some(session) = self
            .state
            .open_session
            .as_ref()
            .filter(|open_session| open_session.session_id() == session_id)
            .map(|open_session| open_session.session().clone())
        else {
            self.state
                .set_status_message("transcript export target is no longer available");
            return;
        };
        let detail = self.state.selected_detail();
        if detail.transcript_rows.is_empty() {
            self.state
                .set_status_message("open transcript has no rows to export");
            return;
        }
        let file_name = transcript_markdown_file_name(&session.title, session.id);
        let export_path = session
            .cwd
            .join(".moyai")
            .join("transcript-exports")
            .join(file_name);
        let markdown = open_transcript_rows_to_markdown(
            &session.title,
            &session.cwd,
            session.id,
            &session.base_url,
            &session.model,
            &detail.transcript_rows,
            &detail.file_changes,
        );
        let result = (|| write_markdown_export_atomic(&export_path, &markdown))();
        match result {
            Ok(()) => self
                .state
                .set_status_message(format!("会話をMarkdownで保存しました: {}", export_path)),
            Err(error) => self
                .state
                .set_status_message(format!("transcript markdown export failed: {error}")),
        }
    }

    pub(crate) fn export_history_markdown_to_path(
        &mut self,
        session_id: SessionId,
        path: Utf8PathBuf,
    ) {
        if !self.state.can_export_history() {
            self.state.set_status_message(
                "history export cannot start while another operation is active",
            );
            return;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("history export target is no longer available");
            return;
        }
        self.state.set_status_message("履歴をMarkdownで保存中…");
        let target = HistoryExportRequestTarget {
            workspace_authority_root: self.app.workspace.authority_root().to_path_buf(),
            session_id,
        };
        let request_id = self.history_export_requests.begin(target.clone());
        self.state.begin_history_export();
        self.spawn_history_markdown_export(
            session_id,
            normalize_markdown_export_path(path),
            request_id,
            target,
        );
    }

    fn spawn_history_markdown_export(
        &self,
        session_id: SessionId,
        export_path: Utf8PathBuf,
        request_id: LatestRequestId,
        target: HistoryExportRequestTarget,
    ) {
        let service = self.app.session_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop history-export runtime");
            let path = export_path.clone();
            let result = runtime
                .block_on(async move {
                    let read = canonical_markdown_export_read(&service, session_id).await?;
                    if read.history.items.is_empty() {
                        return Err(crate::error::SessionError::Message(
                            "canonical protocol history is empty".to_string(),
                        ));
                    }
                    Ok::<_, crate::error::SessionError>(read)
                })
                .map_err(|error| error.to_string())
                .and_then(|read| {
                    let markdown = canonical_session_read_to_markdown(&read);
                    write_markdown_export_atomic(&export_path, &markdown)?;
                    Ok(path)
                });
            let _ = runtime_tx.send(RuntimeMessage::HistoryExported {
                request_id,
                target,
                result,
            });
        });
    }
}
