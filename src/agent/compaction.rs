use crate::agent::event::StreamAccumulator;
use crate::agent::prompt::{AgentRunRequest, build_provider_replay_messages_from_history_items};
use crate::agent::prompt_assets::render_compaction_prompt;
use crate::error::AgentError;
use crate::llm::{ChatRequest, LlmClient, ModelContentPart, ModelMessage};
use crate::protocol::{HistoryItem, HistoryItemPayload};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, MessageMetadata, MessagePart, MessageRole, NewMessage, NewPart, PartKind,
    SessionRecord, SessionRepository, TodoItem,
};
use crate::storage::SqliteSessionRepository;
use crate::tool::truncate::clip_text_with_ellipsis;

const MAX_USER_TURNS_BEFORE_SUMMARY: usize = 2;
const MAX_COMPACTION_TARGETS: usize = 3;

pub async fn maybe_compact(
    llm: &dyn LlmClient,
    session_repo: &SqliteSessionRepository,
    request: &AgentRunRequest,
    todos: &[TodoItem],
    sink: &mut dyn RunEventSink,
) -> Result<bool, AgentError> {
    if !needs_compaction(request) {
        return Ok(false);
    }

    let split_index = match compaction_split_index(request) {
        Some(value) => value,
        None => return Ok(false),
    };

    let summary_messages = build_compaction_messages_from_history_items(
        &request.session.session,
        &request.runtime_input.history_items[..split_index],
    );
    if summary_messages.is_empty() {
        return Ok(false);
    }

    let todo_block = if todos.is_empty() {
        "No active todo list was recorded.".to_string()
    } else {
        todos
            .iter()
            .map(|todo| {
                format!(
                    "- [{}] {} ({:?})",
                    todo_status_label(todo),
                    todo.content,
                    todo.priority
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let continuation_block = continuation_focus_block(request, todos);

    let mut accumulator = StreamAccumulator::default();
    let prompt_profile = request
        .config
        .model
        .prompt_profile
        .resolved_for_model(&request.model.name);
    let response = llm
        .stream_chat(
            ChatRequest {
                model: request.model.clone(),
                base_url: request.config.model.base_url.clone(),
                system_prompt: render_compaction_prompt(
                    prompt_profile,
                    &todo_block,
                    &continuation_block,
                ),
                messages: summary_messages.clone(),
                tools: Vec::new(),
                timeout_ms: request.config.model.request_timeout_ms,
                stream_idle_timeout_ms: request.config.model.stream_idle_timeout_ms,
                extra_headers: request.config.model.extra_headers.clone(),
                temperature: request.config.model.temperature,
                top_p: request.config.model.top_p,
                top_k: request.config.model.top_k,
                presence_penalty: request.config.model.presence_penalty,
                frequency_penalty: request.config.model.frequency_penalty,
                seed: request.config.model.seed,
                stop_sequences: request.config.model.stop_sequences.clone(),
                extra_body: request.config.model.extra_body_json.clone(),
            },
            request.cancel.clone(),
            &mut accumulator,
        )
        .await?;

    let summary_text = match accumulator.text.trim() {
        "" => deterministic_compaction_summary(
            &request.session.session,
            split_index,
            &summary_messages,
            &todo_block,
            &continuation_block,
        ),
        text => text.to_string(),
    };

    let message = session_repo
        .append_message(
            NewMessage {
                session_id: request.session.session.id,
                parent_message_id: Some(request.user_message_id),
                role: MessageRole::Assistant,
                metadata: MessageMetadata::Assistant(AssistantMessageMeta {
                    model: request.model.name.clone(),
                    base_url: request.config.model.base_url.clone(),
                    finish_reason: Some(response.finish_reason),
                    token_usage: response.usage.clone(),
                    summary: true,
                }),
            },
            vec![NewPart {
                kind: PartKind::Text,
                payload: MessagePart::Text(crate::session::TextPart {
                    text: summary_text.clone(),
                }),
            }],
        )
        .await?;

    let continuation = compaction_continuation_contract(request, todos);
    sink.emit(crate::session::RunEvent::CompactionCompleted {
        message_id: message.id,
        summarized_messages: split_index,
        summary: summary_text,
        continuation,
    })?;

    Ok(true)
}

pub fn needs_compaction(request: &AgentRunRequest) -> bool {
    let history_items = &request.runtime_input.history_items;
    if history_items.is_empty() {
        return false;
    }
    if unsummarized_user_turns_from_history_items(history_items) > MAX_USER_TURNS_BEFORE_SUMMARY {
        return true;
    }
    let reserved = request
        .model
        .max_output_tokens
        .saturating_add(request.config.session.overflow_margin_tokens as u32);
    let available = request.model.context_window.saturating_sub(reserved);
    if available == 0 {
        return false;
    }
    estimate_history_item_tokens(history_items) >= available as usize
}

fn compaction_split_index(request: &AgentRunRequest) -> Option<usize> {
    let history_items = &request.runtime_input.history_items;
    if history_items.is_empty() {
        return None;
    }
    let latest_summary = latest_summary_history_index(history_items);
    let start = latest_summary.map(|index| index + 1).unwrap_or(0);
    if unsummarized_user_turns_from_history_items(history_items) > MAX_USER_TURNS_BEFORE_SUMMARY {
        if let Some(split) = latest_user_turn_history_index_after_summary(history_items, start) {
            if split > start {
                return Some(split);
            }
        }
    }
    let items = &history_items[start..];
    if items.len() <= 4 {
        return None;
    }

    let preserve_recent_cap = request
        .config
        .session
        .transcript_limit_messages
        .clamp(8, 24);
    let reserved = request
        .model
        .max_output_tokens
        .saturating_add(request.config.session.overflow_margin_tokens as u32);
    let available = request.model.context_window.saturating_sub(reserved) as usize;
    let recent_token_budget = (available / 2).max(1_024);

    let mut keep_count = 0usize;
    let mut recent_tokens = 0usize;
    for item in items.iter().rev() {
        let next_count = keep_count + 1;
        let next_tokens = recent_tokens + estimate_history_item_token(item);
        if next_count > 4 && (next_count > preserve_recent_cap || next_tokens > recent_token_budget)
        {
            break;
        }
        keep_count = next_count;
        recent_tokens = next_tokens;
    }

    if keep_count >= items.len() {
        keep_count = 4;
    }

    let split = history_items.len().saturating_sub(keep_count);
    if split <= start { None } else { Some(split) }
}

fn unsummarized_user_turns_from_history_items(history_items: &[HistoryItem]) -> usize {
    let start = latest_summary_history_index(history_items)
        .map(|index| index + 1)
        .unwrap_or(0);
    history_items[start..]
        .iter()
        .filter(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
        .count()
}

fn latest_user_turn_history_index_after_summary(
    history_items: &[HistoryItem],
    start: usize,
) -> Option<usize> {
    history_items[start..]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, item)| {
            matches!(item.payload, HistoryItemPayload::UserTurn { .. }).then_some(start + offset)
        })
}

fn latest_summary_history_index(history_items: &[HistoryItem]) -> Option<usize> {
    history_items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| {
            matches!(item.payload, HistoryItemPayload::Compaction { .. }).then_some(index)
        })
}

fn build_compaction_messages_from_history_items(
    session: &SessionRecord,
    history_items: &[HistoryItem],
) -> Vec<ModelMessage> {
    build_provider_replay_messages_from_history_items(session, history_items, history_items.len())
}

fn estimate_history_item_tokens(history_items: &[HistoryItem]) -> usize {
    history_items.iter().map(estimate_history_item_token).sum()
}

fn estimate_history_item_token(item: &HistoryItem) -> usize {
    serde_json::to_string(&item.payload)
        .map(|text| estimate_text_tokens(&text))
        .unwrap_or(1)
}

fn estimate_text_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

fn clip_compaction_text(text: &str, limit: usize) -> String {
    let normalized = text.trim().replace('\t', " ");
    if normalized.len() <= limit {
        return normalized;
    }
    clip_text_with_ellipsis(&normalized, limit)
}

fn continuation_focus_block(request: &AgentRunRequest, todos: &[TodoItem]) -> String {
    let _ = todos;
    let mut lines = vec![
        format!("Route: {}", task_route_label(request.state.route)),
        format!(
            "Phase: {}",
            process_phase_label(request.state.process_phase)
        ),
    ];

    let active_targets = request
        .state
        .active_targets
        .iter()
        .map(|value| value.as_str())
        .collect::<Vec<_>>();
    if !active_targets.is_empty() {
        let mut targets = active_targets
            .iter()
            .take(MAX_COMPACTION_TARGETS)
            .copied()
            .collect::<Vec<_>>();
        if active_targets.len() > MAX_COMPACTION_TARGETS {
            targets.push("and more targets");
        }
        lines.push(format!("Targets: {}", targets.join(", ")));
    }
    if let Some(failure) = &request.state.failure {
        lines.push(format!(
            "Repair focus: {}",
            clip_compaction_text(&failure.summary, 180)
        ));
    }
    if let Some(reason) = &request.state.completion.blocked_reason {
        lines.push(format!("Completion gate: {reason}"));
    }
    if let Some(summary) = &request.state.completion.route_contract_summary {
        lines.push(format!("Docs contract: {summary}"));
    }
    if let Some(contract) = request
        .state
        .implementation_handoff
        .as_ref()
        .and_then(|handoff| handoff.continuation_contract.as_ref())
    {
        lines.push("Typed continuation contract:".to_string());
        lines.push(format!(
            "- route={} phase={}",
            contract.route, contract.process_phase
        ));
        if let Some(kind) = &contract.active_work_kind {
            lines.push(format!("- active_work={kind}"));
        }
        if !contract.target_files.is_empty() {
            lines.push(format!(
                "- targets={}",
                contract
                    .target_files
                    .iter()
                    .take(MAX_COMPACTION_TARGETS)
                    .map(|target| target.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !contract.verification_commands.is_empty() {
            lines.push(format!(
                "- verification={}",
                contract.verification_commands.join(" | ")
            ));
        }
        if !contract.invariant_refs.is_empty() {
            lines.push(format!(
                "- invariants={}",
                contract.invariant_refs.join(", ")
            ));
        }
    }
    lines.join("\n")
}

fn deterministic_compaction_summary(
    session: &SessionRecord,
    summarized_messages: usize,
    summary_messages: &[ModelMessage],
    todo_block: &str,
    continuation_block: &str,
) -> String {
    let transcript_excerpt = summary_messages
        .iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(compaction_message_excerpt)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Compaction summary for session `{}`.\nSummarized history items: {}.\nContinuation invariant: CompactionContinuity.\nRecent compacted transcript excerpt:\n{}\nCurrent todo state:\n{}\nContinuation focus:\n{}",
        session.title, summarized_messages, transcript_excerpt, todo_block, continuation_block
    )
}

fn compaction_message_excerpt(message: &ModelMessage) -> String {
    match message {
        ModelMessage::System { content } => {
            format!("system: {}", clip_compaction_text(content, 240))
        }
        ModelMessage::User { content } => format!("user: {}", clip_compaction_text(content, 240)),
        ModelMessage::UserParts { parts } => format!(
            "user: {}",
            clip_compaction_text(&model_content_parts_excerpt(parts), 240)
        ),
        ModelMessage::Assistant { content } => {
            format!("assistant: {}", clip_compaction_text(content, 240))
        }
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => format!(
            "assistant tool calls: {} [{} calls]",
            clip_compaction_text(content.as_deref().unwrap_or(""), 180),
            tool_calls.len()
        ),
        ModelMessage::Tool {
            tool_name, result, ..
        } => {
            format!("tool {tool_name}: {}", clip_compaction_text(result, 240))
        }
    }
}

fn model_content_parts_excerpt(parts: &[ModelContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ModelContentPart::Text { text } => text.clone(),
            ModelContentPart::Image { mime_type, .. } => format!("[image:{mime_type}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compaction_continuation_contract(
    request: &AgentRunRequest,
    todos: &[TodoItem],
) -> Option<crate::session::ContinuationContract> {
    let _ = todos;
    if let Some(contract) = request
        .state
        .implementation_handoff
        .as_ref()
        .and_then(|handoff| handoff.continuation_contract.clone())
    {
        return Some(contract);
    }

    let active_work_summary = request
        .state
        .completion
        .blocked_reason
        .as_ref()
        .cloned()
        .or_else(|| {
            request
                .state
                .failure
                .as_ref()
                .map(|failure| clip_compaction_text(&failure.summary, 180))
        });
    let mut target_files = request.state.active_targets.clone();
    target_files.truncate(MAX_COMPACTION_TARGETS);
    let verification_commands = request.state.verification.required_commands.clone();
    let has_continuity_payload = active_work_summary.is_some()
        || !target_files.is_empty()
        || !verification_commands.is_empty()
        || request.state.failure.is_some()
        || request.state.completion.blocked_reason.is_some();
    if !has_continuity_payload {
        return None;
    }

    Some(crate::session::ContinuationContract {
        route: task_route_label(request.state.route).to_string(),
        process_phase: process_phase_label(request.state.process_phase).to_string(),
        active_work_kind: active_work_summary
            .as_ref()
            .map(|_| "typed_continuation".to_string()),
        active_work_summary,
        required_next_action: None,
        target_files,
        verification_commands,
        failure_kind: request
            .state
            .failure
            .as_ref()
            .map(|failure| format!("{:?}", failure.kind)),
        failure_summary: request
            .state
            .failure
            .as_ref()
            .map(|failure| clip_compaction_text(&failure.summary, 240)),
        completion_blocker: request.state.completion.blocked_reason.clone(),
        invariant_refs: vec!["CompactionContinuity".to_string()],
    })
}

fn task_route_label(route: crate::session::TaskRoute) -> &'static str {
    match route {
        crate::session::TaskRoute::Code => "code",
        crate::session::TaskRoute::Docs => "docs",
        crate::session::TaskRoute::Review => "review",
        crate::session::TaskRoute::Debug => "debug",
        crate::session::TaskRoute::Ask => "ask",
        crate::session::TaskRoute::Summary => "summary",
    }
}

fn process_phase_label(phase: crate::session::ProcessPhase) -> &'static str {
    match phase {
        crate::session::ProcessPhase::Discover => "discover",
        crate::session::ProcessPhase::Author => "author",
        crate::session::ProcessPhase::Verify => "verify",
        crate::session::ProcessPhase::Repair => "repair",
        crate::session::ProcessPhase::Closeout => "closeout",
    }
}

fn todo_status_label(todo: &TodoItem) -> &'static str {
    match todo.status {
        crate::session::TodoStatus::Pending => "pending",
        crate::session::TodoStatus::InProgress => "in_progress",
        crate::session::TodoStatus::Blocked => "blocked",
        crate::session::TodoStatus::Completed => "completed",
        crate::session::TodoStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    use crate::protocol::{ContentPart, HistoryItemId, TurnId};
    use crate::session::{ProjectId, SessionId, SessionStatus};

    fn user_item(session_id: SessionId, turn_id: TurnId, sequence_no: i64) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: sequence_no,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: format!("turn {sequence_no}"),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        }
    }

    fn assistant_item(session_id: SessionId, turn_id: TurnId, sequence_no: i64) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: sequence_no,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: format!("answer {sequence_no}"),
                }],
            },
        }
    }

    fn session_record(session_id: SessionId) -> SessionRecord {
        SessionRecord {
            id: session_id,
            project_id: ProjectId::new(),
            title: "compaction test".to_string(),
            status: SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "test-model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    #[test]
    fn user_turn_threshold_selects_older_history_for_compaction() {
        let session_id = SessionId::new();
        let mut items = Vec::new();
        for turn in 0..4 {
            let turn_id = TurnId::new();
            items.push(user_item(session_id, turn_id, turn * 2 + 1));
            items.push(assistant_item(session_id, turn_id, turn * 2 + 2));
        }

        assert_eq!(unsummarized_user_turns_from_history_items(&items), 4);
        let session = session_record(session_id);
        let summary_messages = build_compaction_messages_from_history_items(&session, &items[..6]);

        assert!(!summary_messages.is_empty());
    }

    #[test]
    fn deterministic_fallback_summary_keeps_continuity_marker() {
        let summary = deterministic_compaction_summary(
            &session_record(SessionId::new()),
            6,
            &[
                ModelMessage::User {
                    content: "1+1".to_string(),
                },
                ModelMessage::Assistant {
                    content: "2".to_string(),
                },
            ],
            "No active todo list was recorded.",
            "No active work state requires special continuation.",
        );

        assert!(summary.contains("Summarized history items: 6"));
        assert!(summary.contains("CompactionContinuity"));
        assert!(summary.contains("user: 1+1"));
        assert!(summary.contains("assistant: 2"));
    }
}
