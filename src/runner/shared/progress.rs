//! Public progress is a bounded projection of the existing durable history owner.
use super::journal::Entry;
use super::protocol::ProgressItem;
use super::{Controller, Report, ReportOutcome, RunnerError};
use crate::protocol::{ContentPart, HistoryItemPayload, ProtocolPage, ProtocolPageRequest, TurnId};
use serde_json::json;

const HISTORY_TAIL: usize = 64;
const MAX_ITEMS: usize = 12;
const MAX_BYTES: usize = 64 * 1024;

impl Controller {
    pub(super) async fn publish_progress(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        let snapshot = self.host.snapshot(entry.run_id)?;
        if !matches!(
            snapshot.state,
            crate::runner::LocalRunState::Running | crate::runner::LocalRunState::WaitingApproval
        ) {
            return Ok(());
        }
        let Some(session_id) = snapshot.session_id else {
            return Ok(());
        };
        // A lost ACK must resend the identical bounded packet even if newer private
        // history moves its public records out of the canonical discovery page.
        if entry.progress_report.is_none() {
            let canonical = self
                .host
                .inner
                .process
                .store()
                .session_repo()
                .canonical_session_protocol_snapshot(
                    session_id,
                    ProtocolPageRequest::Latest {
                        limit: HISTORY_TAIL,
                    },
                    ProtocolPageRequest::Latest { limit: 1 },
                )
                .await
                .map_err(|error| RunnerError::new(error.to_string()))?;
            let Some((turn_id, _)) = canonical.active_turn_position else {
                return Ok(());
            };
            let Some(outcome) = public_tail(canonical.protocol.history, session_id, turn_id)?
            else {
                return Ok(());
            };
            let ReportOutcome::Progress { revision, .. } = &outcome else {
                unreachable!()
            };
            let revision = *revision;
            if revision <= entry.progress_revision {
                return Ok(());
            }
            let report =
                Report::for_assignment(&entry.assignment, &format!("progress:{revision}"), outcome);
            self.journal.queue_progress(entry, report)?;
        }
        let report = entry
            .progress_report
            .as_ref()
            .expect("queued progress delivery")
            .clone();
        let ReportOutcome::Progress { revision, .. } = &report.outcome else {
            return Err(RunnerError::new("Invalid pending progress delivery"));
        };
        let revision = *revision;
        self.client.report(&report).await?;
        self.journal.progress_reported(entry, revision)
    }
}

fn text(value: &str, limit: usize, truncated: &mut bool) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    *truncated |= end < value.len();
    value[..end].to_owned()
}

fn public_payload(
    payload: &HistoryItemPayload,
    truncated: &mut bool,
) -> Option<(&'static str, serde_json::Value)> {
    Some(match payload {
        HistoryItemPayload::AssistantMessage { content, .. } => {
            let mut body = String::new();
            for part in content {
                match part {
                    ContentPart::Text { text: value } => {
                        body.push_str(&text(
                            value,
                            2048usize.saturating_sub(body.len()),
                            truncated,
                        ));
                    }
                    ContentPart::Image { .. } => *truncated = true,
                }
            }
            if body.is_empty() {
                return None;
            }
            (
                "assistant_message",
                json!({"content":[{"kind":"text","text":body}]}),
            )
        }
        HistoryItemPayload::ToolCall {
            call_id,
            tool_name,
            arguments_json,
            ..
        } => (
            "tool_call",
            json!({"call_id":call_id,"tool_name":text(tool_name,256,truncated),
                "arguments_json":text(arguments_json,2048,truncated)}),
        ),
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            success,
            ..
        } => (
            "tool_output",
            json!({"call_id":call_id,"status":status,"title":text(title,256,truncated),
                "output_text":text(output_text,2048,truncated),"success":success}),
        ),
        HistoryItemPayload::Error { message } => {
            ("error", json!({"message":text(message,2048,truncated)}))
        }
        HistoryItemPayload::FileChange { summary, .. } => (
            "file_change",
            json!({"summary":text(summary,2048,truncated)}),
        ),
        // Provider diagnostics, developer instructions, internal context and user
        // attachment snapshots never become public execution progress.
        _ => return None,
    })
}

fn public_tail(
    page: ProtocolPage<crate::protocol::HistoryItem>,
    session_id: crate::session::SessionId,
    turn_id: TurnId,
) -> Result<Option<ReportOutcome>, RunnerError> {
    let mut truncated = page.offset > 0;
    let mut items = Vec::new();
    for (index, item) in page.items.into_iter().enumerate() {
        if item.session_id != session_id || item.turn_id() != Some(turn_id) {
            continue;
        }
        if let Some((kind, payload)) = public_payload(&item.payload, &mut truncated) {
            items.push(ProgressItem {
                position: (page.offset + index + 1) as u64,
                kind: kind.into(),
                payload,
            });
        }
    }
    if items.len() > MAX_ITEMS {
        items.drain(..items.len() - MAX_ITEMS);
        truncated = true;
    }
    let Some(revision) = items.last().map(|item| item.position) else {
        return Ok(None);
    };
    loop {
        let outcome = ReportOutcome::Progress {
            revision,
            items: items.clone(),
            truncated,
        };
        if serde_json::to_vec(&outcome)
            .map_err(|error| RunnerError::new(error.to_string()))?
            .len()
            <= MAX_BYTES
        {
            return Ok(Some(outcome));
        }
        items.remove(0);
        truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HistoryItem, HistoryItemId, HistoryScope, ModelResponseId, TurnId};

    fn item(
        session_id: crate::session::SessionId,
        turn_id: TurnId,
        payload: HistoryItemPayload,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 1,
            payload,
        }
    }

    #[test]
    fn progress_is_public_bounded_and_keeps_canonical_positions() {
        let session_id = crate::session::SessionId::new();
        let turn_id = TurnId::new();
        let mut items = vec![item(
            session_id,
            turn_id,
            HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "private input".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        )];
        for _ in 0..40 {
            items.push(item(
                session_id,
                turn_id,
                HistoryItemPayload::AssistantMessage {
                    response_id: ModelResponseId::new(),
                    content: vec![ContentPart::Text {
                        text: "\0あ".repeat(10000),
                    }],
                },
            ));
        }
        let outcome = public_tail(
            ProtocolPage {
                offset: 100,
                limit: 64,
                total: 141,
                items,
                next_cursor: None,
            },
            session_id,
            turn_id,
        )
        .unwrap()
        .unwrap();
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert!(bytes.len() <= MAX_BYTES);
        assert!(!String::from_utf8(bytes).unwrap().contains("private input"));
        let ReportOutcome::Progress {
            revision,
            items,
            truncated,
        } = outcome
        else {
            panic!()
        };
        assert_eq!(revision, 141);
        assert!(truncated);
        assert!(items.len() <= MAX_ITEMS && !items.is_empty());
        assert_eq!(items.last().unwrap().position, revision);
        for item in items {
            assert!(serde_json::to_vec(&item).unwrap().len() <= 16 * 1024);
        }
    }

    #[test]
    fn progress_preserves_failed_tool_status_without_private_metadata() {
        let mut truncated = false;
        let (_, payload) = public_payload(
            &HistoryItemPayload::ToolOutput {
                call_id: crate::session::ToolCallId::new(),
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Command".into(),
                output_text: "failed".into(),
                success: Some(false),
                metadata: json!({"private":"not public progress"}),
            },
            &mut truncated,
        )
        .unwrap();
        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["success"], false);
        assert!(payload.get("metadata").is_none());
        assert!(!truncated);
    }

    #[test]
    fn progress_excludes_prior_turn_and_other_session_history() {
        let session = crate::session::SessionId::new();
        let turn = TurnId::new();
        let items = vec![
            item(
                session,
                TurnId::new(),
                HistoryItemPayload::Error {
                    message: "previous turn".into(),
                },
            ),
            item(
                crate::session::SessionId::new(),
                turn,
                HistoryItemPayload::Error {
                    message: "other session".into(),
                },
            ),
            item(
                session,
                turn,
                HistoryItemPayload::Error {
                    message: "current turn".into(),
                },
            ),
        ];
        let outcome = public_tail(
            ProtocolPage {
                offset: 0,
                limit: 64,
                total: 3,
                items,
                next_cursor: None,
            },
            session,
            turn,
        )
        .unwrap()
        .unwrap();
        let ReportOutcome::Progress {
            items, revision, ..
        } = outcome
        else {
            panic!()
        };
        assert_eq!(revision, 3);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].payload["message"], "current turn");
    }
}
