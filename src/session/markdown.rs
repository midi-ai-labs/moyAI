use serde_json::Value;

use crate::config::sanitize_provider_endpoint;
use crate::error::SessionError;
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemPayload, ToolLifecycleStatus, TurnItemPayload,
    TurnTerminalOutcome, turn_items_in_projection_order,
};
use crate::session::{
    CanonicalHistoryPage, CanonicalSessionRead, CanonicalTurnPage, RequestDiagnosticsPart,
    SessionId, SessionService, ToolCallId,
};

const MARKDOWN_EXPORT_PAGE_LIMIT: usize = crate::protocol::MAX_PROTOCOL_PAGE_LIMIT;
const MARKDOWN_EXPORT_SNAPSHOT_ATTEMPTS: usize = 3;

/// Captures an explicit full-export artifact through bounded SQL pages.
///
/// Normal UI/session reads remain paged. Export is the one operation allowed to assemble all
/// canonical items, and it verifies the append fence before returning so pages from different
/// revisions are never silently combined.
pub async fn canonical_markdown_export_read(
    service: &SessionService,
    session_id: SessionId,
) -> Result<CanonicalSessionRead, SessionError> {
    for _ in 0..MARKDOWN_EXPORT_SNAPSHOT_ATTEMPTS {
        let first = service
            .canonical_session_snapshot(
                session_id,
                0,
                MARKDOWN_EXPORT_PAGE_LIMIT,
                0,
                MARKDOWN_EXPORT_PAGE_LIMIT,
            )
            .await?;
        let expected_fence = first.fence;
        let mut history_items = first.read.history.items;
        let mut turn_items = first.read.turns.items;

        while history_items.len() < expected_fence.history_count {
            let page = service
                .canonical_history_page(session_id, history_items.len(), MARKDOWN_EXPORT_PAGE_LIMIT)
                .await?;
            if page.items.is_empty() {
                break;
            }
            history_items.extend(page.items);
        }
        while turn_items.len() < expected_fence.turn_count {
            let page = service
                .canonical_turn_page(session_id, turn_items.len(), MARKDOWN_EXPORT_PAGE_LIMIT)
                .await?;
            if page.items.is_empty() {
                break;
            }
            turn_items.extend(page.items);
        }

        let final_snapshot = service
            .canonical_latest_session_snapshot(session_id, 1, 1)
            .await?;
        if final_snapshot.fence != expected_fence
            || history_items.len() != expected_fence.history_count
            || turn_items.len() != expected_fence.turn_count
        {
            continue;
        }
        let session = final_snapshot.read.session;
        return Ok(CanonicalSessionRead {
            history: CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: history_items.len(),
                total: history_items.len(),
                has_more: false,
                items: history_items,
            },
            turns: CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: turn_items.len(),
                total: turn_items.len(),
                has_more: false,
                items: turn_items,
            },
            turn_elapsed_ms: Default::default(),
            latest_turn_id: final_snapshot.read.latest_turn_id,
            active_turn_id: final_snapshot.read.active_turn_id,
            active_turn_sequence_no: final_snapshot.read.active_turn_sequence_no,
            session,
        });
    }
    Err(SessionError::Message(format!(
        "session {session_id} changed while its Markdown export snapshot was being captured"
    )))
}

pub fn canonical_session_read_to_markdown(read: &CanonicalSessionRead) -> String {
    let session = &read.session;
    let mut events = Vec::new();
    for item in &read.history.items {
        match &item.payload {
            HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::SteerTurn { .. } => {
                let mut body = String::new();
                push_history_user_quote_body(&mut body, item);
                events.push(MarkdownExportEvent::user(body));
            }
            HistoryItemPayload::AssistantMessage { .. }
            | HistoryItemPayload::InterAgentCommunication { .. } => {
                let mut body = String::new();
                push_history_payload(&mut body, &item.payload);
                events.push(MarkdownExportEvent::assistant(body));
            }
            HistoryItemPayload::Error { .. } => {
                events.push(MarkdownExportEvent::detail(
                    "Error",
                    render_history_item_detail(item),
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
    if let Some((status, summary)) = latest_canonical_terminal(read) {
        events.push(MarkdownExportEvent::terminal(status, summary));
    }
    let metadata = history_metadata_lines(session);
    render_codex_turn_block_markdown(&session.title, &events, &metadata)
}

fn latest_canonical_terminal(
    read: &CanonicalSessionRead,
) -> Option<(MarkdownTerminalStatus, String)> {
    let latest_turn_id = read.active_turn_id.or(read.latest_turn_id)?;
    turn_items_in_projection_order(&read.turns.items)
        .into_iter()
        .rev()
        .filter(|item| item.turn_id == latest_turn_id)
        .find_map(|item| match &item.payload {
            TurnItemPayload::Terminal { outcome } => Some((
                match outcome {
                    TurnTerminalOutcome::Completed => MarkdownTerminalStatus::Completed,
                    TurnTerminalOutcome::Failed { .. } => MarkdownTerminalStatus::Failed,
                    TurnTerminalOutcome::Interrupted { .. } => MarkdownTerminalStatus::Interrupted,
                },
                outcome.summary().to_string(),
            )),
            _ => None,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownTerminalStatus {
    Completed,
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

fn push_history_item(output: &mut String, item: &HistoryItem) {
    let role = match &item.payload {
        HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::SteerTurn { .. } => Some("User"),
        HistoryItemPayload::AssistantMessage { .. } => Some("Assistant"),
        HistoryItemPayload::InterAgentCommunication { .. } => Some("Assistant"),
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
        HistoryItemPayload::WorldState { .. } => "World State",
        HistoryItemPayload::ApprovalDecision { .. } => "Approval Decision",
        HistoryItemPayload::InterAgentCommunication { .. } => "Sub-agent Message",
        HistoryItemPayload::SubAgentActivity { .. } => "Sub-agent Activity",
        HistoryItemPayload::CollaborationModeInstruction { .. } => "Collaboration Mode",
        HistoryItemPayload::Compaction { .. } => "Compaction",
        HistoryItemPayload::Error { .. } => "Error",
        HistoryItemPayload::UserTurn { .. }
        | HistoryItemPayload::SteerTurn { .. }
        | HistoryItemPayload::AssistantMessage { .. } => "Message",
    }
}

fn push_history_user_quote_body(output: &mut String, item: &HistoryItem) {
    match &item.payload {
        HistoryItemPayload::UserTurn { content, .. }
        | HistoryItemPayload::SteerTurn { content, .. } => push_content_parts(output, content),
        _ => {}
    }
}

fn history_metadata_lines(session: &crate::session::SessionRecord) -> Vec<MarkdownMetadataLine> {
    let mut lines = vec![
        MarkdownMetadataLine::new("Session ID", session.id.to_string()),
        MarkdownMetadataLine::new("Status", format!("{:?}", session.status)),
        MarkdownMetadataLine::new("Workspace", session.cwd.as_str()),
        MarkdownMetadataLine::new("Model", session.model.clone()),
        MarkdownMetadataLine::new("Base URL", sanitize_provider_endpoint(&session.base_url)),
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

fn push_history_payload(output: &mut String, payload: &HistoryItemPayload) {
    match payload {
        HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            editor_context,
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
        }
        HistoryItemPayload::SteerTurn {
            content,
            additional_context,
            client_user_message_id,
            ..
        } => {
            push_content_parts(output, content);
            if !additional_context.is_empty() || client_user_message_id.is_some() {
                output.push_str("### Active-Turn Steer Context\n\n");
                let context = serde_json::json!({
                    "client_user_message_id": client_user_message_id,
                    "additional_context": additional_context,
                });
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(&context).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::AssistantMessage { content, .. } => {
            push_content_parts(output, content);
        }
        HistoryItemPayload::InterAgentCommunication { communication } => {
            output.push_str("### Sub-agent Message\n\n");
            push_metadata_line(output, "Author", &communication.author);
            push_metadata_line(output, "Recipient", &communication.recipient);
            push_metadata_line(
                output,
                "Trigger turn",
                &communication.trigger_turn.to_string(),
            );
            output.push('\n');
            output.push_str(&communication.content);
            output.push_str("\n\n");
        }
        HistoryItemPayload::SubAgentActivity {
            activity_id,
            agent_session_id,
            agent_path,
            activity_kind,
        } => {
            output.push_str("### Sub-agent Activity\n\n");
            push_metadata_line(output, "Activity ID", activity_id);
            push_metadata_line(output, "Agent Session ID", &agent_session_id.to_string());
            push_metadata_line(output, "Agent path", agent_path);
            push_metadata_line(output, "Activity", &format!("{activity_kind:?}"));
            output.push('\n');
        }
        HistoryItemPayload::CollaborationModeInstruction { mode } => {
            output.push_str("### Collaboration Mode\n\n");
            push_metadata_line(output, "Mode", mode.as_str());
            output.push('\n');
        }
        HistoryItemPayload::Error { message, .. } => {
            output.push_str("### Error\n\n");
            output.push_str(message);
            output.push_str("\n\n");
        }
        HistoryItemPayload::ToolCall {
            call_id,
            model_call_id,
            tool_name,
            arguments_json,
            ..
        } => push_tool_call(
            output,
            *call_id,
            Some(model_call_id),
            tool_name,
            arguments_json,
        ),
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            metadata,
            success,
        } => push_tool_output(
            output,
            *call_id,
            *status,
            title,
            output_text,
            *success,
            Some(metadata),
        ),
        HistoryItemPayload::RequestDiagnostics { diagnostics } => {
            push_request_diagnostics(output, diagnostics);
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
        HistoryItemPayload::WorldState { snapshot, rendered } => {
            output.push_str("### World State\n\n");
            output.push_str(rendered);
            output.push_str("\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(snapshot).unwrap_or_default(),
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
        HistoryItemPayload::Compaction { summary, .. } => {
            output.push_str("### Compaction\n\n");
            output.push_str(summary);
            output.push_str("\n\n");
        }
    }
}

fn push_tool_call(
    output: &mut String,
    call_id: ToolCallId,
    model_call_id: Option<&str>,
    tool_name: &str,
    arguments_json: &str,
) {
    output.push_str("### Tool Call: ");
    output.push_str(tool_name);
    output.push_str("\n\n");
    output.push_str("- Tool call ID: `");
    output.push_str(&call_id.to_string());
    output.push_str("`\n");
    if let Some(model_call_id) = model_call_id.filter(|value| !value.is_empty()) {
        output.push_str("- Model call ID: `");
        output.push_str(model_call_id);
        output.push_str("`\n");
    }
    output.push('\n');
    let rendered = serde_json::from_str::<Value>(arguments_json)
        .and_then(|arguments| serde_json::to_string_pretty(&arguments))
        .unwrap_or_else(|_| arguments_json.to_string());
    push_fenced(output, "json", &rendered);
}

fn push_tool_output(
    output: &mut String,
    call_id: ToolCallId,
    status: ToolLifecycleStatus,
    title: &str,
    output_text: &str,
    success: Option<bool>,
    metadata: Option<&Value>,
) {
    output.push_str("### Tool Result: ");
    output.push_str(title);
    output.push_str("\n\n");
    output.push_str("- Tool call ID: `");
    output.push_str(&call_id.to_string());
    output.push_str("`\n");
    output.push_str("- Status: `");
    output.push_str(&format!("{status:?}"));
    output.push_str("`\n");
    if let Some(success) = success {
        output.push_str("- Success: `");
        output.push_str(&success.to_string());
        output.push_str("`\n");
    }
    output.push('\n');
    output.push_str(output_text);
    output.push_str("\n\n");
    if let Some(metadata) = metadata.filter(|value| match value {
        Value::Null => false,
        Value::Object(values) => !values.is_empty(),
        _ => true,
    }) {
        output.push_str("#### Metadata\n\n");
        push_fenced(
            output,
            "json",
            &serde_json::to_string_pretty(metadata).unwrap_or_else(|_| metadata.to_string()),
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

fn push_request_diagnostics(output: &mut String, value: &RequestDiagnosticsPart) {
    output.push_str("### Request Diagnostics\n\n");
    push_metadata_line(output, "Provider", &value.provider);
    push_metadata_line(output, "Model", &value.model_name);
    push_metadata_line(
        output,
        "Base URL",
        &sanitize_provider_endpoint(&value.base_url),
    );
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
        "Prepared Model Message Count",
        &value.provider_message_count.to_string(),
    );
    if let Some(wire) = &value.wire {
        push_metadata_line(output, "Wire Transport", &wire.transport);
        push_metadata_line(output, "Wire API Mode", &wire.api_mode);
        push_metadata_line(output, "Wire Input Kind", &wire.input_kind);
        push_metadata_line(output, "Wire Input Count", &wire.input_count.to_string());
        push_metadata_line(
            output,
            "Wire Serialized Body Bytes",
            &wire.serialized_body_bytes.to_string(),
        );
        push_metadata_line(
            output,
            "Wire Continuation Present",
            &wire.continuation_present.to_string(),
        );
    }
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
    if let Some(context_window) = &value.context_window {
        push_metadata_line(
            output,
            "Active Context Token Source",
            context_window.source.key(),
        );
        push_metadata_line(
            output,
            "Active Context Tokens (estimated)",
            &context_window.active_context_tokens.to_string(),
        );
        push_metadata_line(
            output,
            "Tokens Until Context Limit (estimated)",
            &context_window.tokens_until_limit.to_string(),
        );
        push_metadata_line(
            output,
            "Context Limit Reached",
            &context_window.token_limit_reached.to_string(),
        );
    }
    output.push('\n');
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

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::AccessMode;
    use crate::protocol::{
        ContentPart, HistoryItemId, HistoryScope, InterAgentCommunication, ModelResponseId,
        SubAgentActivityKind, TurnId, TurnItem, TurnItemId,
    };
    use crate::session::{
        CanonicalHistoryPage, CanonicalTurnPage, ProjectId, SessionId, SessionModelParameters,
        SessionRecord, SessionStatus,
    };

    #[test]
    fn request_diagnostics_markdown_distinguishes_prepared_history_from_wire_input() {
        let diagnostics: RequestDiagnosticsPart = serde_json::from_value(serde_json::json!({
            "provider": "openai_compat",
            "model_name": "fixture-model",
            "base_url": "http://localhost:1234/v1",
            "request_timeout_ms": 30_000,
            "stream_idle_timeout_ms": 30_000,
            "system_prompt_chars": 10,
            "tool_count": 0,
            "provider_message_count": 19,
            "messages": [],
            "context_window": {
                "source": "provider_usage_with_local_estimate",
                "active_context_tokens": 1234,
                "full_context_window_limit": 32768,
                "configured_max_output_tokens": 512,
                "overflow_margin_tokens": 128,
                "tokens_until_limit": 30894,
                "token_limit_reached": false
            },
            "wire": {
                "transport": "http",
                "api_mode": "responses",
                "input_kind": "input_items",
                "input_count": 7,
                "serialized_body_bytes": 12_345,
                "continuation_present": false
            }
        }))
        .expect("request diagnostics fixture");
        let mut markdown = String::new();

        push_request_diagnostics(&mut markdown, &diagnostics);

        assert!(markdown.contains("Prepared Model Message Count: 19"));
        assert!(
            markdown.contains("Active Context Token Source: provider_usage_with_local_estimate")
        );
        assert!(markdown.contains("Wire API Mode: responses"));
        assert!(markdown.contains("Wire Input Kind: input_items"));
        assert!(markdown.contains("Wire Input Count: 7"));
        assert!(markdown.contains("Wire Serialized Body Bytes: 12345"));
        assert!(markdown.contains("Wire Continuation Present: false"));
    }

    #[test]
    fn history_markdown_preserves_canonical_cross_turn_order() {
        let session = test_session();
        let older = message_item(&session, TurnId::new(), 29, 100, "older-stage");
        let newer = message_item(&session, TurnId::new(), 15, 200, "newer-stage");
        let markdown = canonical_session_read_to_markdown(&canonical_read(
            &session,
            vec![older, newer],
            Vec::new(),
        ));

        let older_position = markdown
            .find("older-stage")
            .expect("older item is exported");
        let newer_position = markdown
            .find("newer-stage")
            .expect("newer item is exported");
        assert!(
            older_position < newer_position,
            "markdown export must preserve canonical input order"
        );
    }

    #[test]
    fn history_markdown_renders_agent_message_and_activity_without_reasoning() {
        let session = test_session();
        let turn_id = TurnId::new();
        let communication = HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            scope: HistoryScope::Session,
            sequence_no: 1,
            created_at_ms: 100,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: InterAgentCommunication {
                    author: "/root/reviewer".to_string(),
                    recipient: "/root".to_string(),
                    content: "review complete".to_string(),
                    trigger_turn: false,
                },
            },
        };
        let activity = HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: 101,
            payload: HistoryItemPayload::SubAgentActivity {
                activity_id: "activity-1".to_string(),
                agent_session_id: SessionId::new(),
                agent_path: "/root/reviewer".to_string(),
                activity_kind: SubAgentActivityKind::Interacted,
            },
        };

        let markdown = canonical_session_read_to_markdown(&canonical_read(
            &session,
            vec![communication, activity],
            Vec::new(),
        ));

        assert!(markdown.contains("### Sub-agent Message"));
        assert!(markdown.contains("review complete"));
        assert!(markdown.contains("### Sub-agent Activity"));
        assert!(markdown.contains("activity-1"));
        assert!(!markdown.contains("### Reasoning"));
    }

    #[test]
    fn history_markdown_pretty_prints_valid_raw_tool_arguments() {
        let session = test_session();
        let turn_id = TurnId::new();
        let tool_call = tool_call_item(
            &session,
            turn_id,
            "vendor.custom_tool",
            r#"{"path":"C:\\workspace","nested":{"value":1}}"#,
        );

        let markdown = canonical_session_read_to_markdown(&canonical_read(
            &session,
            vec![tool_call],
            Vec::new(),
        ));

        assert!(markdown.contains("### Tool Call: vendor.custom_tool"));
        assert!(markdown.contains("\n  \"nested\": {\n    \"value\": 1\n  },"));
        assert!(markdown.contains("\n  \"path\": \"C:\\\\workspace\"\n"));
    }

    #[test]
    fn history_markdown_preserves_invalid_raw_tool_arguments_verbatim() {
        let session = test_session();
        let turn_id = TurnId::new();
        let invalid_arguments = r#"{"path":"unterminated"#;
        let tool_call = tool_call_item(&session, turn_id, "vendor.invalid_tool", invalid_arguments);

        let markdown = canonical_session_read_to_markdown(&canonical_read(
            &session,
            vec![tool_call],
            Vec::new(),
        ));

        assert!(markdown.contains("### Tool Call: vendor.invalid_tool"));
        assert!(markdown.contains(&format!("```json\n{invalid_arguments}\n```")));
    }

    #[test]
    fn history_markdown_uses_canonical_turn_terminal() {
        let mut session = test_session();
        session.status = SessionStatus::Cancelled;
        let turn_id = TurnId::new();
        let terminal = TurnItem {
            id: TurnItemId::new(),
            session_id: session.id,
            turn_id,
            source_item_id: None,
            sequence_no: 2,
            payload: TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
            },
        };
        let read = canonical_read(
            &session,
            vec![message_item(&session, turn_id, 1, 100, "stop this")],
            vec![terminal],
        );

        let markdown = canonical_session_read_to_markdown(&read);

        assert!(markdown.contains("停止しました: run stopped by user"));
    }

    #[test]
    fn session_scoped_mail_does_not_hide_the_latest_real_turn_terminal() {
        let session = test_session();
        let turn_id = TurnId::new();
        let terminal = TurnItem {
            id: TurnItemId::new(),
            session_id: session.id,
            turn_id,
            source_item_id: None,
            sequence_no: 2,
            payload: TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Completed,
            },
        };
        let mail = HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: 200,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: InterAgentCommunication {
                    author: "/root/worker".to_string(),
                    recipient: "/root".to_string(),
                    content: "future evidence".to_string(),
                    trigger_turn: false,
                },
            },
        };
        let read = canonical_read(
            &session,
            vec![message_item(&session, turn_id, 1, 100, "request"), mail],
            vec![terminal],
        );

        assert!(matches!(
            latest_canonical_terminal(&read),
            Some((MarkdownTerminalStatus::Completed, summary)) if summary == "completed"
        ));
        let markdown = canonical_session_read_to_markdown(&read);
        assert!(markdown.contains("future evidence"));
    }

    #[test]
    fn history_markdown_does_not_attach_an_older_terminal_to_a_new_incomplete_turn() {
        let mut session = test_session();
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let completed_turn_id = TurnId::new();
        let incomplete_turn_id = TurnId::new();
        let older_terminal = TurnItem {
            id: TurnItemId::new(),
            session_id: session.id,
            turn_id: completed_turn_id,
            source_item_id: None,
            sequence_no: 2,
            payload: TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
            },
        };
        let read = canonical_read(
            &session,
            vec![
                message_item(&session, completed_turn_id, 1, 100, "older request"),
                message_item(&session, incomplete_turn_id, 1, 200, "new request"),
            ],
            vec![older_terminal],
        );

        let markdown = canonical_session_read_to_markdown(&read);

        assert!(markdown.contains("new request"));
        assert!(!markdown.contains("停止しました: run stopped by user"));
    }

    #[test]
    fn history_markdown_uses_a_later_terminal_only_turn() {
        let mut session = test_session();
        session.status = SessionStatus::Failed;
        let older_turn_id = TurnId::new();
        let terminal_only_turn_id = TurnId::new();
        let older_terminal = TurnItem {
            id: TurnItemId::new(),
            session_id: session.id,
            turn_id: older_turn_id,
            source_item_id: None,
            sequence_no: 2,
            payload: TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Completed,
            },
        };
        let later_terminal = TurnItem {
            id: TurnItemId::new(),
            session_id: session.id,
            turn_id: terminal_only_turn_id,
            source_item_id: None,
            sequence_no: 0,
            payload: TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Failed {
                    error: "startup recovery failed the admitted turn".to_string(),
                },
            },
        };
        let mut read = canonical_read(
            &session,
            vec![message_item(
                &session,
                older_turn_id,
                1,
                100,
                "older request",
            )],
            vec![older_terminal, later_terminal],
        );
        read.latest_turn_id = Some(terminal_only_turn_id);

        assert!(matches!(
            latest_canonical_terminal(&read),
            Some((MarkdownTerminalStatus::Failed, summary))
                if summary == "startup recovery failed the admitted turn"
        ));
        let markdown = canonical_session_read_to_markdown(&read);
        assert!(markdown.contains("失敗しました: startup recovery failed the admitted turn"));
        assert!(!markdown.contains("完了しました。"));
    }

    fn canonical_read(
        session: &SessionRecord,
        history_items: Vec<HistoryItem>,
        turn_items: Vec<TurnItem>,
    ) -> CanonicalSessionRead {
        let latest_turn_id = history_items
            .iter()
            .rev()
            .find_map(HistoryItem::turn_id)
            .or_else(|| turn_items.last().map(|item| item.turn_id));
        CanonicalSessionRead {
            session: session.clone(),
            history: CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: usize::MAX,
                total: history_items.len(),
                has_more: false,
                items: history_items,
            },
            turns: CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: usize::MAX,
                total: turn_items.len(),
                has_more: false,
                items: turn_items,
            },
            turn_elapsed_ms: Default::default(),
            latest_turn_id,
            active_turn_id: None,
            active_turn_sequence_no: None,
        }
    }

    fn message_item(
        session: &SessionRecord,
        turn_id: TurnId,
        sequence_no: i64,
        created_at_ms: i64,
        text: &str,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no,
            created_at_ms,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        }
    }

    fn tool_call_item(
        session: &SessionRecord,
        turn_id: TurnId,
        tool_name: &str,
        arguments_json: &str,
    ) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: session.id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 100,
            payload: HistoryItemPayload::ToolCall {
                call_id: ToolCallId::new(),
                response_id: ModelResponseId::new(),
                model_call_id: "provider-call-1".to_string(),
                tool_name: tool_name.to_string(),
                arguments_json: arguments_json.to_string(),
            },
        }
    }

    fn test_session() -> SessionRecord {
        SessionRecord {
            id: SessionId::new(),
            project_id: ProjectId::new(),
            title: "test".to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "model".to_string(),
            base_url: "http://local".to_string(),
            access_mode: AccessMode::FullAccess,
            model_parameters: SessionModelParameters::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
    }
}
