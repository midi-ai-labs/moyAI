use serde_json::Value;

use crate::protocol::{
    ContentPart, FileChangeEvidence, HistoryItem, HistoryItemId, HistoryItemPayload,
    ToolLifecycleStatus, TurnId,
};
use crate::session::{
    ChangeId, ChangeKind, MessageMetadata, MessagePart, MessageRole, PartKind, PartRecord,
    RequestDiagnosticsPart, SessionId, SessionStatus, ToolCallId, ToolCallStatus, Transcript,
    TranscriptMessage, transcript_from_history_items,
};

pub fn transcript_to_markdown(transcript: &Transcript) -> String {
    let session = &transcript.session;
    let mut output = String::new();
    output.push_str("# ");
    output.push_str(&session.title);
    output.push_str("\n\n");
    push_metadata_line(&mut output, "Session ID", &session.id.to_string());
    push_metadata_line(&mut output, "Status", &format!("{:?}", session.status));
    push_metadata_line(&mut output, "Workspace", session.cwd.as_str());
    push_metadata_line(&mut output, "Model", &session.model);
    push_metadata_line(&mut output, "Base URL", &session.base_url);
    push_metadata_line(
        &mut output,
        "Created At (ms)",
        &session.created_at_ms.to_string(),
    );
    push_metadata_line(
        &mut output,
        "Updated At (ms)",
        &session.updated_at_ms.to_string(),
    );
    if let Some(completed_at_ms) = session.completed_at_ms {
        push_metadata_line(
            &mut output,
            "Completed At (ms)",
            &completed_at_ms.to_string(),
        );
    }
    output.push('\n');

    for message in &transcript.messages {
        push_message(&mut output, message);
    }

    output
}

pub fn history_items_to_markdown(
    session: &crate::session::SessionRecord,
    items: &[HistoryItem],
) -> String {
    let mut events = Vec::new();
    let mut ordered = items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| (item.sequence_no, item.created_at_ms));
    for item in ordered {
        match &item.payload {
            HistoryItemPayload::UserTurn { .. }
            | HistoryItemPayload::Message {
                role: MessageRole::User,
                ..
            } => {
                let mut body = String::new();
                push_history_user_quote_body(&mut body, item);
                events.push(MarkdownExportEvent::user(body));
            }
            HistoryItemPayload::Message {
                role: MessageRole::Assistant,
                ..
            } => {
                let mut body = String::new();
                push_history_payload(&mut body, &item.payload);
                events.push(MarkdownExportEvent::assistant(body));
            }
            HistoryItemPayload::Error { message, .. } => {
                events.push(MarkdownExportEvent::detail(
                    "Error",
                    render_history_item_detail(item),
                ));
                events.push(MarkdownExportEvent::terminal(
                    MarkdownTerminalStatus::Failed,
                    message,
                ));
            }
            _ => {
                events.push(MarkdownExportEvent::detail(
                    history_item_detail_title(item),
                    render_history_item_detail(item),
                ));
            }
        }
    }
    if let Some(status) = markdown_terminal_status_from_session_status(session.status) {
        events.push(MarkdownExportEvent::terminal(
            status,
            markdown_session_terminal_summary(session.status),
        ));
    }
    let metadata = history_metadata_lines(session);
    render_codex_turn_block_markdown(&session.title, &events, &metadata)
}

pub(crate) fn filechange_display_export_preserves_call_id_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let change_id = ChangeId::new();
    let session = crate::session::SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "FileChange owner fixture".to_string(),
        status: SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "fixture-model".to_string(),
        base_url: "http://fixture".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: Some(3),
    };
    let item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::FileChange {
            call_id,
            change_ids: vec![change_id],
            changes: vec![FileChangeEvidence {
                change_id,
                kind: ChangeKind::Update,
                path_before: None,
                path_after: Some(camino::Utf8PathBuf::from("src/lib.rs")),
                summary: "Updated src/lib.rs".to_string(),
            }],
            summary: "Updated src/lib.rs".to_string(),
        },
    };
    let markdown = history_items_to_markdown(&session, std::slice::from_ref(&item));
    if !markdown.contains("Tool Call ID") || !markdown.contains(&call_id.to_string()) {
        return false;
    }
    let transcript = transcript_from_history_items(&session, std::slice::from_ref(&item));
    let transcript_markdown = transcript_to_markdown(&transcript);
    if !transcript_markdown.contains("Tool Call ID")
        || !transcript_markdown.contains(&call_id.to_string())
    {
        return false;
    }
    transcript.messages.iter().any(|message| {
        message.parts.iter().any(|part| {
            part.kind == PartKind::DiffSummary
                && matches!(
                    &part.payload,
                    MessagePart::DiffSummary(diff)
                        if diff.tool_call_id == Some(call_id)
                            && diff.change_ids.as_slice() == [change_id]
                )
        })
    })
}

#[cfg(test)]
pub(crate) fn tooloutput_markdown_export_preserves_blocked_action_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let blocked_action = "apply_patch:src/workflow.rs";
    let session = crate::session::SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "ToolOutput evidence fixture".to_string(),
        status: SessionStatus::Failed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "fixture-model".to_string(),
        base_url: "http://fixture".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: Some(3),
    };
    let item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Blocked,
            title: "Blocked edit".to_string(),
            output_text: "Edit blocked by active work contract.".to_string(),
            metadata: Value::Null,
            success: Some(false),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: Some(blocked_action.to_string()),
            result_hash: Some("blocked-action-fixture".to_string()),
            verification_run: None,
        },
    };
    let markdown = history_items_to_markdown(&session, std::slice::from_ref(&item));
    markdown.contains("Blocked action")
        && markdown.contains(blocked_action)
        && markdown.contains("NoProgress")
        && markdown.contains("blocked-action-fixture")
}

pub(crate) fn session_markdown_legacy_toolcall_arguments_do_not_render_typed_projection_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let message_id = crate::session::MessageId::new();
    let call_id = ToolCallId::new();
    let session = crate::session::SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "Legacy display-only ToolCall fixture".to_string(),
        status: SessionStatus::Completed,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: "fixture-model".to_string(),
        base_url: "http://fixture".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: Some(3),
    };
    let legacy_transcript = Transcript {
        session: session.clone(),
        messages: vec![TranscriptMessage {
            record: crate::session::MessageRecord {
                id: message_id,
                session_id,
                role: MessageRole::Assistant,
                parent_message_id: None,
                sequence_no: 1,
                created_at_ms: 1,
                metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: "fixture-model".to_string(),
                    base_url: "http://fixture".to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: false,
                }),
            },
            parts: vec![PartRecord {
                id: crate::session::PartId::new(),
                message_id,
                sequence_no: 1,
                kind: PartKind::ToolCall,
                payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                    tool_call_id: call_id,
                    tool_name: crate::tool::ToolName::Write,
                    arguments_json: r#"{"path":"src/workflow.rs","content":"display only"}"#
                        .to_string(),
                    model_arguments_json: None,
                    effective_arguments_json: None,
                }),
            }],
        }],
    };
    let legacy_markdown = transcript_to_markdown(&legacy_transcript);
    let typed_transcript = Transcript {
        session,
        messages: vec![TranscriptMessage {
            record: crate::session::MessageRecord {
                id: message_id,
                session_id,
                role: MessageRole::Assistant,
                parent_message_id: None,
                sequence_no: 1,
                created_at_ms: 1,
                metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: "fixture-model".to_string(),
                    base_url: "http://fixture".to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: false,
                }),
            },
            parts: vec![PartRecord {
                id: crate::session::PartId::new(),
                message_id,
                sequence_no: 1,
                kind: PartKind::ToolCall,
                payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                    tool_call_id: call_id,
                    tool_name: crate::tool::ToolName::Write,
                    arguments_json: r#"{"path":"src/workflow.rs","content":"display"}"#.to_string(),
                    model_arguments_json: Some(
                        r#"{"path":"src/workflow.rs","content":"model"}"#.to_string(),
                    ),
                    effective_arguments_json: Some(
                        r#"{"path":"src/workflow.rs","content":"effective"}"#.to_string(),
                    ),
                }),
            }],
        }],
    };
    let typed_markdown = transcript_to_markdown(&typed_transcript);
    legacy_markdown.contains(r#""path": "src/workflow.rs""#)
        && !legacy_markdown.contains("Tool Arguments Projection")
        && !legacy_markdown.contains("effective_arguments")
        && !legacy_markdown.contains("model_arguments")
        && typed_markdown.contains("Tool Arguments Projection")
        && typed_markdown.contains("effective")
        && typed_markdown.contains("model")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownTerminalStatus {
    Completed,
    AwaitingUser,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone)]
pub struct MarkdownExportEvent {
    pub kind: MarkdownExportEventKind,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub enum MarkdownExportEventKind {
    User,
    Assistant,
    Detail,
    Terminal(MarkdownTerminalStatus),
}

impl MarkdownExportEvent {
    pub fn user(body: impl Into<String>) -> Self {
        Self {
            kind: MarkdownExportEventKind::User,
            title: "User".to_string(),
            body: body.into(),
        }
    }

    pub fn assistant(body: impl Into<String>) -> Self {
        Self {
            kind: MarkdownExportEventKind::Assistant,
            title: "Assistant".to_string(),
            body: body.into(),
        }
    }

    pub fn detail(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: MarkdownExportEventKind::Detail,
            title: title.into(),
            body: body.into(),
        }
    }

    pub fn terminal(status: MarkdownTerminalStatus, body: impl Into<String>) -> Self {
        Self {
            kind: MarkdownExportEventKind::Terminal(status),
            title: "Terminal".to_string(),
            body: body.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MarkdownMetadataLine {
    pub label: String,
    pub value: String,
}

impl MarkdownMetadataLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Default)]
struct MarkdownTurnBlock {
    user_body: String,
    details: Vec<MarkdownExportEvent>,
    assistant_bodies: Vec<String>,
    terminal: Option<(MarkdownTerminalStatus, String)>,
}

impl MarkdownTurnBlock {
    fn has_content(&self) -> bool {
        !self.user_body.trim().is_empty()
            || !self.details.is_empty()
            || !self.assistant_bodies.is_empty()
            || self.terminal.is_some()
    }
}

pub fn render_codex_turn_block_markdown(
    title: &str,
    events: &[MarkdownExportEvent],
    metadata: &[MarkdownMetadataLine],
) -> String {
    let mut blocks = Vec::new();
    let mut current = MarkdownTurnBlock::default();
    for event in events {
        match &event.kind {
            MarkdownExportEventKind::User => {
                if current.has_content() {
                    blocks.push(current);
                    current = MarkdownTurnBlock::default();
                }
                current.user_body = event.body.trim().to_string();
            }
            MarkdownExportEventKind::Assistant => {
                if !event.body.trim().is_empty() {
                    current.assistant_bodies.push(event.body.trim().to_string());
                }
            }
            MarkdownExportEventKind::Detail => {
                if !event.body.trim().is_empty() || !event.title.trim().is_empty() {
                    current.details.push(event.clone());
                }
            }
            MarkdownExportEventKind::Terminal(status) => {
                current.terminal = Some((*status, event.body.trim().to_string()));
            }
        }
    }
    if current.has_content() {
        blocks.push(current);
    }

    let mut output = String::new();
    output.push_str("# ");
    output.push_str(&markdown_heading_text(title));
    output.push_str("\n\n");
    for block in &blocks {
        push_turn_block(&mut output, block);
    }
    output.push_str("<details><summary>実行情報</summary>\n\n");
    for line in metadata {
        push_metadata_line(&mut output, &line.label, &line.value);
    }
    output.push_str("</details>\n");
    output
}

pub fn codex_turn_block_markdown_fixture_passes() -> bool {
    let events = vec![
        MarkdownExportEvent::user("first request"),
        MarkdownExportEvent::assistant("first final answer"),
        MarkdownExportEvent::user("second request"),
        MarkdownExportEvent::assistant("I will keep working."),
        MarkdownExportEvent::detail("Work Summary", "- 結果: run cancelled by user"),
        MarkdownExportEvent::terminal(MarkdownTerminalStatus::Interrupted, "run cancelled by user"),
    ];
    let markdown = render_codex_turn_block_markdown(
        "projection fixture",
        &events,
        &[MarkdownMetadataLine::new("Session", "`fixture`")],
    );
    let Some(first_user) = markdown.find("> first request") else {
        return false;
    };
    let Some(first_final) = markdown.find("first final answer") else {
        return false;
    };
    let Some(second_user) = markdown.find("> second request") else {
        return false;
    };
    let Some(intent) = markdown.find("I will keep working.") else {
        return false;
    };
    let Some(terminal) = markdown.find("停止しました: run cancelled by user") else {
        return false;
    };
    first_user < first_final
        && first_final < second_user
        && second_user < intent
        && intent < terminal
}

fn push_turn_block(output: &mut String, block: &MarkdownTurnBlock) {
    if !block.user_body.trim().is_empty() {
        output.push_str("> ");
        output.push_str(&block.user_body.trim().replace('\n', "\n> "));
        output.push_str("\n\n");
    }

    let (final_outcome, assistant_detail_count) = turn_final_outcome(block);
    let detail_count = block.details.len() + assistant_detail_count;
    if detail_count > 0 {
        output.push_str("<details><summary>");
        output.push_str(&format!("{detail_count} previous messages"));
        output.push_str("</summary>\n\n");
        for body in assistant_detail_bodies(block) {
            output.push_str("> ");
            output.push_str(&body.replace('\n', "\n> "));
            output.push_str("\n\n");
        }
        for detail in &block.details {
            push_markdown_detail_event(output, detail);
        }
        output.push_str("</details>\n\n");
    }

    if let Some(final_outcome) = final_outcome {
        if !final_outcome.trim().is_empty() {
            output.push_str(final_outcome.trim());
            output.push_str("\n\n");
        }
    }
}

fn turn_final_outcome(block: &MarkdownTurnBlock) -> (Option<String>, usize) {
    if let Some((status, summary)) = &block.terminal
        && *status != MarkdownTerminalStatus::Completed
    {
        return (
            Some(terminal_outcome_text(*status, summary)),
            block.assistant_bodies.len(),
        );
    }
    let assistant_bodies = block
        .assistant_bodies
        .iter()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if let Some(last) = assistant_bodies.last() {
        return (
            Some((*last).to_string()),
            assistant_bodies.len().saturating_sub(1),
        );
    }
    if let Some((status, summary)) = &block.terminal {
        return (Some(terminal_outcome_text(*status, summary)), 0);
    }
    (None, 0)
}

fn assistant_detail_bodies(block: &MarkdownTurnBlock) -> Vec<String> {
    let assistant_bodies = block
        .assistant_bodies
        .iter()
        .map(|body| body.trim())
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>();
    if block
        .terminal
        .as_ref()
        .is_some_and(|(status, _)| *status != MarkdownTerminalStatus::Completed)
    {
        return assistant_bodies.into_iter().map(str::to_string).collect();
    }
    assistant_bodies
        .into_iter()
        .take(block.assistant_bodies.len().saturating_sub(1))
        .map(str::to_string)
        .collect()
}

fn terminal_outcome_text(status: MarkdownTerminalStatus, summary: &str) -> String {
    let summary = summary.trim();
    match status {
        MarkdownTerminalStatus::Completed
            if summary.is_empty() || summary == "session completed" =>
        {
            "完了しました。".to_string()
        }
        MarkdownTerminalStatus::Completed => summary.to_string(),
        MarkdownTerminalStatus::AwaitingUser
            if summary.is_empty() || summary == "session awaiting user" =>
        {
            "ユーザー確認待ちで停止しました。".to_string()
        }
        MarkdownTerminalStatus::AwaitingUser => {
            format!("ユーザー確認待ちで停止しました: {summary}")
        }
        MarkdownTerminalStatus::Failed if summary.is_empty() => "失敗しました。".to_string(),
        MarkdownTerminalStatus::Failed => format!("失敗しました: {summary}"),
        MarkdownTerminalStatus::Interrupted if summary.is_empty() => "停止しました。".to_string(),
        MarkdownTerminalStatus::Interrupted => format!("停止しました: {summary}"),
    }
}

fn push_markdown_detail_event(output: &mut String, event: &MarkdownExportEvent) {
    let title = event.title.trim();
    let body = event.body.trim();
    output.push_str("<details><summary>");
    output.push_str(&markdown_heading_text(if title.is_empty() {
        "作業履歴"
    } else {
        title
    }));
    output.push_str("</summary>\n\n");
    if body.is_empty() {
        output.push_str("_内容はありません。_\n\n");
    } else {
        output.push_str(body);
        output.push_str("\n\n");
    }
    output.push_str("</details>\n\n");
}

pub fn history_markdown_file_name(title: &str, session_id: SessionId) -> String {
    let slug = title_slug(title);
    let id = session_id.to_string();
    let short_id = id.get(..10).unwrap_or(&id);
    format!("moyai-history-{slug}-{short_id}.md")
}

fn push_message(output: &mut String, message: &TranscriptMessage) {
    let role = match message.record.role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
    };
    output.push_str("## ");
    output.push_str(role);
    output.push_str(" Message ");
    output.push_str(&message.record.sequence_no.to_string());
    output.push_str("\n\n");

    if message.parts.is_empty() {
        output.push_str("_No recorded parts._\n\n");
        return;
    }

    for part in &message.parts {
        if let Some(payload) = materialized_history_payload_for_part(message, part) {
            push_history_payload(output, &payload);
            continue;
        }
        match &part.payload {
            MessagePart::Error(value) => {
                output.push_str("### Error\n\n");
                output.push_str("- Category: `");
                output.push_str(&format!("{:?}", value.category));
                output.push_str("`\n\n");
                output.push_str(&value.message);
                output.push_str("\n\n");
            }
            MessagePart::DiffSummary(value) => {
                output.push_str("### Diff Summary\n\n");
                if let Some(call_id) = value.tool_call_id {
                    push_metadata_line(output, "Tool Call ID", &call_id.to_string());
                    output.push('\n');
                }
                output.push_str(&value.summary);
                output.push_str("\n\n");
            }
            MessagePart::PromptDispatch(value) => {
                output.push_str("### Prompt Dispatch\n\n");
                push_labeled_text(output, "Raw prompt", &value.raw_prompt_text);
                push_labeled_text(output, "Dispatched prompt", &value.dispatch_prompt_text);
                if let Some(draft) = &value.enhanced_draft_text {
                    push_labeled_text(output, "Enhanced draft", draft);
                }
                if !value.transforms.is_empty() {
                    output.push_str("Transforms:\n");
                    for transform in &value.transforms {
                        output.push_str("- `");
                        output.push_str(&format!("{:?}", transform.kind));
                        output.push('`');
                        if let Some(label) = &transform.label {
                            output.push_str(": ");
                            output.push_str(label);
                        }
                        output.push('\n');
                    }
                    output.push('\n');
                }
                if let Some(error) = &value.transform_error {
                    push_labeled_text(output, "Transform error", error);
                }
            }
            MessagePart::RequestDiagnostics(value) => {
                push_request_diagnostics(output, value);
            }
            MessagePart::Text(_)
            | MessagePart::Image(_)
            | MessagePart::Reasoning(_)
            | MessagePart::ToolCall(_)
            | MessagePart::ToolResult(_) => {}
        }
    }
}

fn push_history_item(output: &mut String, item: &HistoryItem) {
    let role = match &item.payload {
        HistoryItemPayload::UserTurn { .. } => Some("User"),
        HistoryItemPayload::Message { role, .. } => Some(match role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
        }),
        _ => None,
    };
    if let Some(role) = role {
        output.push_str("## ");
        output.push_str(role);
        output.push_str(" Message ");
        output.push_str(&item.sequence_no.to_string());
        output.push_str("\n\n");
    }
    push_history_payload(output, &item.payload);
}

fn render_history_item_detail(item: &HistoryItem) -> String {
    let mut output = String::new();
    push_history_item(&mut output, item);
    output.trim().to_string()
}

fn history_item_detail_title(item: &HistoryItem) -> &'static str {
    match &item.payload {
        HistoryItemPayload::ToolCall { .. } => "Tool Call",
        HistoryItemPayload::ToolOutput { .. } => "Tool Result",
        HistoryItemPayload::RequestDiagnostics { .. } => "Request Diagnostics",
        HistoryItemPayload::FileChange { .. } => "File Changes",
        HistoryItemPayload::Reasoning { .. } => "Reasoning",
        HistoryItemPayload::PromptDispatch { .. } => "Prompt Dispatch",
        HistoryItemPayload::RejectedToolProposal { .. } => "Rejected Tool Proposal",
        HistoryItemPayload::CandidateRepairEdit { .. } => "Candidate Repair Edit",
        HistoryItemPayload::Continuation { .. } => "Continuation",
        HistoryItemPayload::StateProjection { .. } | HistoryItemPayload::SessionState { .. } => {
            "State"
        }
        HistoryItemPayload::LifecycleGuard { .. } => "Lifecycle Guard",
        HistoryItemPayload::ApprovalDecision { .. } => "Approval Decision",
        HistoryItemPayload::RetryDecision { .. } => "Retry Decision",
        HistoryItemPayload::ControlEnvelope { .. } => "Control Envelope",
        HistoryItemPayload::Compaction { .. } => "Compaction",
        HistoryItemPayload::Error { .. } => "Error",
        HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::Message { .. } => "Message",
    }
}

fn push_history_user_quote_body(output: &mut String, item: &HistoryItem) {
    match &item.payload {
        HistoryItemPayload::UserTurn { content, .. }
        | HistoryItemPayload::Message {
            role: MessageRole::User,
            content,
            ..
        } => push_content_parts(output, content),
        _ => {}
    }
}

fn history_metadata_lines(session: &crate::session::SessionRecord) -> Vec<MarkdownMetadataLine> {
    let mut lines = vec![
        MarkdownMetadataLine::new("Session ID", session.id.to_string()),
        MarkdownMetadataLine::new("Status", format!("{:?}", session.status)),
        MarkdownMetadataLine::new("Workspace", session.cwd.as_str()),
        MarkdownMetadataLine::new("Model", session.model.clone()),
        MarkdownMetadataLine::new("Base URL", session.base_url.clone()),
        MarkdownMetadataLine::new("Created At (ms)", session.created_at_ms.to_string()),
        MarkdownMetadataLine::new("Updated At (ms)", session.updated_at_ms.to_string()),
    ];
    if let Some(completed_at_ms) = session.completed_at_ms {
        lines.push(MarkdownMetadataLine::new(
            "Completed At (ms)",
            completed_at_ms.to_string(),
        ));
    }
    lines
}

fn markdown_terminal_status_from_session_status(
    status: SessionStatus,
) -> Option<MarkdownTerminalStatus> {
    match status {
        SessionStatus::Completed => Some(MarkdownTerminalStatus::Completed),
        SessionStatus::AwaitingUser => Some(MarkdownTerminalStatus::AwaitingUser),
        SessionStatus::Cancelled => Some(MarkdownTerminalStatus::Interrupted),
        SessionStatus::Failed => Some(MarkdownTerminalStatus::Failed),
        SessionStatus::Running | SessionStatus::Idle => None,
    }
}

fn markdown_session_terminal_summary(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Completed => "session completed",
        SessionStatus::AwaitingUser => "session awaiting user",
        SessionStatus::Cancelled => "run cancelled by user",
        SessionStatus::Failed => "session failed",
        SessionStatus::Running | SessionStatus::Idle => "",
    }
}

fn materialized_history_payload_for_part(
    message: &TranscriptMessage,
    part: &PartRecord,
) -> Option<HistoryItemPayload> {
    match &part.payload {
        MessagePart::Text(value) => Some(HistoryItemPayload::Message {
            message_id: Some(message.record.id),
            role: message.record.role,
            content: vec![ContentPart::Text {
                text: value.text.clone(),
            }],
        }),
        MessagePart::Image(value) => Some(HistoryItemPayload::Message {
            message_id: Some(message.record.id),
            role: message.record.role,
            content: vec![ContentPart::Image {
                image: value.clone(),
            }],
        }),
        MessagePart::Reasoning(value) => Some(HistoryItemPayload::Reasoning {
            text: value.text.clone(),
        }),
        MessagePart::ToolCall(value) => Some(HistoryItemPayload::ToolCall {
            call_id: value.tool_call_id,
            tool: value.tool_name.clone(),
            arguments: serde_json::from_str(&value.arguments_json)
                .unwrap_or_else(|_| Value::String(value.arguments_json.clone())),
            model_arguments: value
                .model_arguments_json
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null),
            effective_arguments: value
                .effective_arguments_json
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        }),
        MessagePart::ToolResult(value) => Some(HistoryItemPayload::ToolOutput {
            call_id: value.tool_call_id,
            status: tool_lifecycle_status_from_session_status(value.status),
            title: value.title.clone(),
            output_text: value.summary.clone(),
            metadata: Value::Null,
            success: value.success,
            progress_effect: value.progress_effect.clone(),
            blocked_action: value.blocked_action.clone(),
            result_hash: value.result_hash.clone(),
            verification_run: None,
        }),
        MessagePart::RequestDiagnostics(value) => Some(HistoryItemPayload::RequestDiagnostics {
            diagnostics: value.clone(),
        }),
        MessagePart::DiffSummary(value) => {
            let call_id = value.tool_call_id?;
            Some(HistoryItemPayload::FileChange {
                call_id,
                change_ids: value.change_ids.clone(),
                changes: value.changes.clone(),
                summary: value.summary.clone(),
            })
        }
        MessagePart::PromptDispatch(value) => Some(HistoryItemPayload::PromptDispatch {
            dispatch: value.clone(),
            editor_context: match &message.record.metadata {
                MessageMetadata::User(meta) => meta.editor_context.clone(),
                _ => None,
            },
        }),
        MessagePart::Error(value) => Some(HistoryItemPayload::Error {
            message_id: Some(message.record.id),
            message: value.message.clone(),
        }),
    }
}

fn push_history_payload(output: &mut String, payload: &HistoryItemPayload) {
    match payload {
        HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            editor_context,
            turn_context,
            ..
        } => {
            push_content_parts(output, content);
            if let Some(prompt_dispatch) = prompt_dispatch {
                output.push_str("### Prompt Dispatch\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(prompt_dispatch).unwrap_or_default(),
                );
            }
            if let Some(editor_context) = editor_context {
                output.push_str("### Editor Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(editor_context).unwrap_or_default(),
                );
            }
            if let Some(turn_context) = turn_context {
                output.push_str("### Turn Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(turn_context).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::Message { content, .. } => {
            push_content_parts(output, content);
        }
        HistoryItemPayload::Error { message, .. } => {
            output.push_str("### Error\n\n");
            output.push_str(message);
            output.push_str("\n\n");
        }
        HistoryItemPayload::Reasoning { text } => {
            output.push_str("### Reasoning\n\n");
            output.push_str(text);
            output.push_str("\n\n");
        }
        HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            model_arguments,
            effective_arguments,
            adjusted_arguments,
            permission_decision,
            sandbox_decision,
            allowed_surface,
            retry_policy,
            terminal_guard_policy,
        } => {
            output.push_str("### Tool Call: ");
            output.push_str(&tool.to_string());
            output.push_str("\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n\n");
            let rendered =
                serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
            push_fenced(output, "json", &rendered);
            if !model_arguments.is_null()
                || !effective_arguments.is_null()
                || adjusted_arguments.is_some()
            {
                output.push_str("#### Tool Arguments Projection\n\n");
                let projection = serde_json::json!({
                    "model_arguments": model_arguments,
                    "effective_arguments": effective_arguments,
                    "adjusted_arguments": adjusted_arguments,
                });
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(&projection).unwrap_or_default(),
                );
            }
            if permission_decision.is_some()
                || sandbox_decision.is_some()
                || !allowed_surface.is_empty()
                || retry_policy.is_some()
                || terminal_guard_policy.is_some()
            {
                output.push_str("#### Tool Lifecycle Decisions\n\n");
                let decisions = serde_json::json!({
                    "permission_decision": permission_decision,
                    "sandbox_decision": sandbox_decision,
                    "allowed_surface": allowed_surface,
                    "retry_policy": retry_policy,
                    "terminal_guard_policy": terminal_guard_policy,
                });
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(&decisions).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            verification_run,
            success,
            progress_effect,
            blocked_action,
            result_hash,
            ..
        } => {
            output.push_str("### Tool Result: ");
            output.push_str(title);
            output.push_str("\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n");
            output.push_str("- Status: `");
            output.push_str(&format!("{status:?}"));
            output.push_str("`\n\n");
            if let Some(success) = success {
                output.push_str("- Success: `");
                output.push_str(&success.to_string());
                output.push_str("`\n");
            }
            output.push_str("- Progress effect: `");
            output.push_str(&format!("{progress_effect:?}"));
            output.push_str("`\n");
            if let Some(blocked_action) = blocked_action {
                push_metadata_line(output, "Blocked action", blocked_action);
            }
            if let Some(hash) = result_hash {
                output.push_str("- Result hash: `");
                output.push_str(hash);
                output.push_str("`\n");
            }
            output.push('\n');
            output.push_str(output_text);
            output.push_str("\n\n");
            if let Some(verification_run) = verification_run {
                output.push_str("#### Verification Run\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(verification_run).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::RequestDiagnostics { diagnostics } => {
            push_request_diagnostics(output, diagnostics);
        }
        HistoryItemPayload::PromptDispatch {
            dispatch,
            editor_context,
        } => {
            output.push_str("### Prompt Dispatch\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(dispatch).unwrap_or_default(),
            );
            if let Some(editor_context) = editor_context {
                output.push_str("### Editor Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(editor_context).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::FileChange {
            call_id,
            summary,
            changes,
            ..
        } => {
            output.push_str("### Diff Summary\n\n");
            push_metadata_line(output, "Tool Call ID", &call_id.to_string());
            output.push('\n');
            output.push_str(summary);
            output.push_str("\n\n");
            if !changes.is_empty() {
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(changes).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::RejectedToolProposal { proposal } => {
            output.push_str("### Rejected Tool Proposal\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(proposal).unwrap_or_default(),
            );
        }
        HistoryItemPayload::CandidateRepairEdit { candidate } => {
            output.push_str("### Candidate Repair Edit\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(candidate).unwrap_or_default(),
            );
        }
        HistoryItemPayload::Continuation { contract } => {
            output.push_str("### Continuation\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(contract).unwrap_or_default(),
            );
        }
        HistoryItemPayload::StateProjection { projection } => {
            output.push_str("### State Projection\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(projection).unwrap_or_default(),
            );
        }
        HistoryItemPayload::SessionState { state } => {
            output.push_str("### Session State\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(state).unwrap_or_default(),
            );
        }
        HistoryItemPayload::ApprovalDecision { call_id, decision } => {
            output.push_str("### Approval Decision\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(decision).unwrap_or_default(),
            );
        }
        HistoryItemPayload::RetryDecision {
            attempt,
            message,
            next_retry_at_ms,
        } => {
            output.push_str("### Retry Decision\n\n");
            output.push_str("- Attempt: `");
            output.push_str(&attempt.to_string());
            output.push_str("`\n");
            output.push_str("- Next retry at ms: `");
            output.push_str(&next_retry_at_ms.to_string());
            output.push_str("`\n\n");
            output.push_str(message);
            output.push_str("\n\n");
        }
        HistoryItemPayload::ControlEnvelope { envelope } => {
            output.push_str("### Control Envelope\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(envelope).unwrap_or_default(),
            );
        }
        HistoryItemPayload::LifecycleGuard { snapshot } => {
            output.push_str("### Lifecycle Guard\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(snapshot).unwrap_or_default(),
            );
        }
        HistoryItemPayload::Compaction { summary, .. } => {
            output.push_str("### Compaction\n\n");
            output.push_str(summary);
            output.push_str("\n\n");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn filechange_display_export_preserves_call_id() {
        assert!(super::filechange_display_export_preserves_call_id_fixture_passes());
    }

    #[test]
    fn tooloutput_markdown_export_preserves_blocked_action() {
        assert!(super::tooloutput_markdown_export_preserves_blocked_action_fixture_passes());
    }

    #[test]
    fn session_markdown_legacy_toolcall_arguments_do_not_render_typed_projection() {
        assert!(
            super::session_markdown_legacy_toolcall_arguments_do_not_render_typed_projection_fixture_passes()
        );
    }
}

fn push_content_parts(output: &mut String, content: &[ContentPart]) {
    for content in content {
        match content {
            ContentPart::Text { text } => {
                output.push_str(text);
                output.push_str("\n\n");
            }
            ContentPart::Image { image } => {
                output.push_str("### Image Attachment\n\n");
                if let Some(path) = &image.source_path {
                    push_metadata_line(output, "Source", path.as_str());
                }
                push_metadata_line(output, "MIME Type", &image.mime_type);
                push_metadata_line(output, "Bytes", &image.byte_len.to_string());
                output.push('\n');
            }
        }
    }
}

fn tool_lifecycle_status_from_session_status(status: ToolCallStatus) -> ToolLifecycleStatus {
    match status {
        ToolCallStatus::Pending => ToolLifecycleStatus::Pending,
        ToolCallStatus::Running => ToolLifecycleStatus::Running,
        ToolCallStatus::Completed => ToolLifecycleStatus::Completed,
        ToolCallStatus::Failed => ToolLifecycleStatus::Failed,
    }
}

fn push_request_diagnostics(output: &mut String, value: &RequestDiagnosticsPart) {
    output.push_str("### Request Diagnostics\n\n");
    push_metadata_line(output, "Provider", &value.provider);
    push_metadata_line(output, "Model", &value.model_name);
    push_metadata_line(output, "Base URL", &value.base_url);
    push_metadata_line(
        output,
        "Request Timeout (ms)",
        &value.request_timeout_ms.to_string(),
    );
    push_metadata_line(
        output,
        "Stream Idle Timeout (ms)",
        &value.stream_idle_timeout_ms.to_string(),
    );
    push_metadata_line(
        output,
        "Stream Max Retries",
        &value.stream_max_retries.to_string(),
    );
    if let Some(supports_tools) = value.supports_tools {
        push_metadata_line(output, "Supports Tools", &supports_tools.to_string());
    }
    if let Some(supports_reasoning) = value.supports_reasoning {
        push_metadata_line(
            output,
            "Supports Reasoning",
            &supports_reasoning.to_string(),
        );
    }
    if let Some(supports_images) = value.supports_images {
        push_metadata_line(output, "Supports Images", &supports_images.to_string());
    }
    push_metadata_line(output, "Tool Count", &value.tool_count.to_string());
    push_metadata_line(
        output,
        "Provider Message Count",
        &value.provider_message_count.to_string(),
    );
    if value.image_count > 0 {
        push_metadata_line(output, "Image Count", &value.image_count.to_string());
        push_metadata_line(output, "Image Bytes", &value.image_bytes.to_string());
    }
    if !value.tool_names.is_empty() {
        push_metadata_line(output, "Tools", &value.tool_names.join(", "));
    }
    if let Some(parallel_tool_calls) = value.parallel_tool_calls {
        push_metadata_line(
            output,
            "Parallel Tool Calls",
            &parallel_tool_calls.to_string(),
        );
    }
    if let Some(control) = &value.control_envelope {
        push_metadata_line(output, "Control Projection ID", &control.projection_id);
        push_metadata_line(output, "Control Policy", &control.dispatch_policy);
        push_metadata_line(output, "Control Validation", &control.validation_status);
        if !control.allowed_tools.is_empty() {
            push_metadata_line(
                output,
                "Allowed Control Tools",
                &control.allowed_tools.join(", "),
            );
        }
    }
    output.push('\n');
}

fn push_labeled_text(output: &mut String, label: &str, value: &str) {
    output.push_str("**");
    output.push_str(label);
    output.push_str("**\n\n");
    output.push_str(value);
    output.push_str("\n\n");
}

fn push_metadata_line(output: &mut String, label: &str, value: &str) {
    output.push_str("- ");
    output.push_str(label);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn push_fenced(output: &mut String, language: &str, value: &str) {
    let fence = markdown_fence_for(value);
    output.push_str(&fence);
    if !language.is_empty() {
        output.push_str(language);
    }
    output.push('\n');
    output.push_str(value);
    if !value.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&fence);
    output.push_str("\n\n");
}

fn markdown_heading_text(value: &str) -> String {
    value
        .lines()
        .next()
        .unwrap_or("Transcript")
        .replace('#', "\\#")
        .trim()
        .to_string()
}

fn markdown_fence_for(value: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in value.chars() {
        if ch == '`' {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(max_run.max(2) + 1)
}

fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}
