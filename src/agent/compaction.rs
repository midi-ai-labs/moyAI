use std::borrow::Cow;

use crate::agent::event::StreamAccumulator;
use crate::agent::prompt::{AgentRunRequest, build_provider_replay_messages_from_history_items};
use crate::agent::prompt_assets::render_compaction_prompt;
use crate::error::AgentError;
use crate::llm::{ChatRequest, LlmClient, ModelContentPart, ModelMessage};
use crate::protocol::{ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, TurnId};
use crate::runtime::RunEventSink;
use crate::session::{
    AssistantMessageMeta, MessageMetadata, MessagePart, MessageRole, NewMessage, NewPart, PartKind,
    SessionId, SessionRecord, SessionRepository, TodoItem, TokenAccountingSource,
    TokenAccountingState,
};
use crate::storage::SqliteSessionRepository;
use crate::tool::truncate::clip_text_with_ellipsis;

const AUTO_COMPACT_CONTEXT_WINDOW_PERCENT: usize = 90;
const MAX_COMPACTION_TARGETS: usize = 3;
const COMPACTION_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const COMPACTION_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";

pub async fn maybe_compact(
    llm: &dyn LlmClient,
    session_repo: &SqliteSessionRepository,
    request: &AgentRunRequest,
    todos: &[TodoItem],
    sink: &mut dyn RunEventSink,
) -> Result<bool, AgentError> {
    let canonical_history_items =
        canonical_history_items_for_compaction(&request.runtime_input.history_items);
    let history_items = canonical_history_items.as_ref();
    if !needs_compaction_for_history_items(request, history_items) {
        return Ok(false);
    }

    let split_index = match compaction_split_index_for_history_items(request, history_items) {
        Some(value) => value,
        None => return Ok(false),
    };

    let summary_messages = build_compaction_messages_from_history_items(
        &request.session.session,
        &history_items[..split_index],
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
                tool_choice: None,
                parallel_tool_calls: false,
                timeout_ms: request.config.model.request_timeout_ms,
                stream_idle_timeout_ms: request.config.model.stream_idle_timeout_ms,
                stream_max_retries: request.config.model.stream_max_retries,
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
    let summary_text =
        compaction_summary_with_continuity(summary_text, split_index, &continuation_block);

    let continuation = compaction_continuation_contract(request, todos);
    let (_message, compaction_event) = session_repo
        .append_message_with_parts_and_protocol_event(
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
            |message_id| crate::session::RunEvent::CompactionCompleted {
                message_id,
                summarized_messages: split_index,
                summary: summary_text.clone(),
                continuation,
            },
            request.protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;

    sink.emit_pre_recorded(compaction_event)?;
    persist_compaction_token_accounting(session_repo, request, &summary_text, sink).await?;

    Ok(true)
}

async fn persist_compaction_token_accounting(
    session_repo: &SqliteSessionRepository,
    request: &AgentRunRequest,
    summary_text: &str,
    sink: &mut dyn RunEventSink,
) -> Result<(), AgentError> {
    let mut state = session_repo.get_state(request.session.session.id).await?;
    let estimated_tokens = estimate_compaction_summary_replay_tokens(summary_text);
    state.token_accounting = TokenAccountingState::from_replay_estimate(
        request.model.context_window,
        estimated_tokens,
        TokenAccountingSource::CompactionRecomputed,
    );
    let event = crate::session::RunEvent::StateUpdated {
        session_id: request.session.session.id,
        state: state.clone(),
    };
    session_repo
        .update_state_with_protocol_event(
            request.session.session.id,
            &state,
            &event,
            request.protocol_turn_id,
            sink.reserve_protocol_sequence_no(),
        )
        .await?;
    sink.emit_pre_recorded(event)?;
    Ok(())
}

pub fn needs_compaction(request: &AgentRunRequest) -> bool {
    let canonical_history_items =
        canonical_history_items_for_compaction(&request.runtime_input.history_items);
    needs_compaction_for_history_items(request, canonical_history_items.as_ref())
}

fn needs_compaction_for_history_items(
    request: &AgentRunRequest,
    history_items: &[HistoryItem],
) -> bool {
    if history_items.is_empty() {
        return false;
    }
    let Some(limit) = auto_compact_token_limit(request.model.context_window) else {
        return false;
    };
    compaction_trigger_pressure_tokens_for_history_items(request, history_items) >= limit
}

fn compaction_pressure_history_items(history_items: &[HistoryItem]) -> Vec<HistoryItem> {
    let canonical_history_items = canonical_history_items_for_compaction(history_items);
    let ordered = canonical_history_items.as_ref();
    let start = latest_summary_history_index(ordered).unwrap_or(0);
    ordered[start..].to_vec()
}

fn compaction_split_index_for_history_items(
    request: &AgentRunRequest,
    history_items: &[HistoryItem],
) -> Option<usize> {
    if history_items.is_empty() {
        return None;
    }
    let latest_summary = latest_summary_history_index(history_items);
    let start = latest_summary.map(|index| index + 1).unwrap_or(0);
    let items = &history_items[start..];
    if items.len() <= 4 {
        return None;
    }

    let auto_compact_limit = auto_compact_token_limit(request.model.context_window)?;
    let preserve_recent_cap = request
        .config
        .session
        .transcript_limit_messages
        .clamp(8, 24);
    let recent_token_budget = (auto_compact_limit / 2).clamp(1, auto_compact_limit);

    let mut keep_count = 0usize;
    for _item in items.iter().rev() {
        let next_count = keep_count + 1;
        let split_candidate = history_items.len().saturating_sub(next_count);
        let next_tokens = estimate_provider_replay_tokens(
            &request.session.session,
            &history_items[split_candidate..],
        );
        if next_count > 4 && (next_count > preserve_recent_cap || next_tokens > recent_token_budget)
        {
            break;
        }
        keep_count = next_count;
    }

    if keep_count >= items.len() {
        keep_count = 4;
    }

    let split = history_items.len().saturating_sub(keep_count);
    if split <= start { None } else { Some(split) }
}

fn canonical_history_items_for_compaction(history_items: &[HistoryItem]) -> Cow<'_, [HistoryItem]> {
    if history_items_in_canonical_order(history_items) {
        return Cow::Borrowed(history_items);
    }
    let mut ordered = history_items.to_vec();
    ordered.sort_by_key(history_item_order_key);
    Cow::Owned(ordered)
}

fn history_items_in_canonical_order(history_items: &[HistoryItem]) -> bool {
    history_items
        .windows(2)
        .all(|pair| history_item_order_key(&pair[0]) <= history_item_order_key(&pair[1]))
}

fn history_item_order_key(item: &HistoryItem) -> (i64, i64) {
    (item.sequence_no, item.created_at_ms)
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

fn auto_compact_token_limit(context_window: u32) -> Option<usize> {
    if context_window == 0 {
        return None;
    }
    Some((context_window as usize).saturating_mul(AUTO_COMPACT_CONTEXT_WINDOW_PERCENT) / 100)
}

fn compaction_trigger_pressure_tokens_for_history_items(
    request: &AgentRunRequest,
    history_items: &[HistoryItem],
) -> usize {
    let pressure_items = compaction_pressure_history_items(history_items);
    let provider_visible_tokens =
        estimate_provider_replay_tokens(&request.session.session, &pressure_items);
    compaction_pressure_with_accounting(provider_visible_tokens, &request.state.token_accounting)
}

fn compaction_pressure_with_accounting(
    provider_visible_tokens: usize,
    accounting: &TokenAccountingState,
) -> usize {
    let accounted_tokens = accounting.active_context_tokens.min(usize::MAX as u64) as usize;
    provider_visible_tokens.max(accounted_tokens)
}

fn compaction_trigger_provider_visible_tokens(
    session: &SessionRecord,
    history_items: &[HistoryItem],
) -> usize {
    let pressure_items = compaction_pressure_history_items(history_items);
    estimate_provider_replay_tokens(session, &pressure_items)
}

fn estimate_provider_replay_tokens(
    session: &SessionRecord,
    history_items: &[HistoryItem],
) -> usize {
    estimate_model_message_tokens(&build_compaction_messages_from_history_items(
        session,
        history_items,
    ))
}

fn estimate_model_message_tokens(messages: &[ModelMessage]) -> usize {
    messages.iter().map(estimate_model_message_token).sum()
}

fn estimate_model_message_token(message: &ModelMessage) -> usize {
    serde_json::to_string(message)
        .map(|text| estimate_text_tokens(&text))
        .unwrap_or(1)
}

fn estimate_compaction_summary_replay_tokens(summary: &str) -> usize {
    estimate_model_message_token(&ModelMessage::System {
        content: summary.to_string(),
    })
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
    text.len().div_ceil(4).max(1)
}

pub(crate) fn compaction_trigger_ignores_pre_summary_history_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let old_huge = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 30,
        payload: HistoryItemPayload::Message {
            message_id: None,
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "x".repeat(600_000),
            }],
        },
    };
    let compacted_old_id = old_huge.id;
    let summary = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 10,
        payload: HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::PreTurn,
            summary: "CompactionContinuity: compacted older large history.".to_string(),
            replacement_item_ids: vec![compacted_old_id],
            continuation: None,
        },
    };
    let current_user = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 20,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "write the missing doc".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };
    let current_assistant = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::Message {
            message_id: None,
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "I will write it.".to_string(),
            }],
        },
    };
    let history = vec![old_huge, summary, current_user, current_assistant];
    let full_tokens = estimate_history_item_tokens(&history);
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compaction trigger fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: COMPACTION_FIXTURE_MODEL.to_string(),
        base_url: COMPACTION_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let pressure_tokens = compaction_trigger_provider_visible_tokens(&session, &history);

    full_tokens > 130_000
        && pressure_tokens < 1_024
        && compaction_pressure_history_items(&history).len() == 3
        && auto_compact_token_limit(131_072) == Some(117_964)
}

pub(crate) fn compaction_trigger_uses_canonical_history_order_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let old_huge = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::Message {
            message_id: None,
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "x".repeat(600_000),
            }],
        },
    };
    let compacted_old_id = old_huge.id;
    let summary = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::PreTurn,
            summary: "CompactionContinuity: compacted older large history.".to_string(),
            replacement_item_ids: vec![compacted_old_id],
            continuation: None,
        },
    };
    let current_user = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "continue after compaction".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };
    let history = vec![summary, current_user, old_huge];
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compaction trigger canonical order fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: camino::Utf8PathBuf::from("C:/workspace"),
        model: COMPACTION_FIXTURE_MODEL.to_string(),
        base_url: COMPACTION_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let pressure_items = compaction_pressure_history_items(&history);
    let pressure_tokens = compaction_trigger_provider_visible_tokens(&session, &history);

    pressure_items.len() == 2 && pressure_tokens < 1_024
}

pub(crate) fn llm_summary_text_is_wrapped_with_typed_continuity_fixture_passes() -> bool {
    let summary = compaction_summary_with_continuity(
        "Older work was summarized.".to_string(),
        12,
        "Route: code\nPhase: repair\nTargets: src/workflow.rs",
    );
    summary.contains("Summarized history items: 12")
        && summary.contains("CompactionContinuity")
        && summary.contains("Continuation focus:")
        && summary.contains("Phase: repair")
        && summary.contains("Older work was summarized.")
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

pub(crate) fn compaction_summary_with_continuity(
    summary_text: String,
    summarized_messages: usize,
    continuation_block: &str,
) -> String {
    let trimmed = summary_text.trim();
    let canonical_prefix = format!(
        "Summarized history items: {summarized_messages}.\nContinuation invariant: CompactionContinuity.\nContinuation focus:\n{}\nCompacted summary:",
        continuation_block.trim()
    );
    if trimmed.starts_with(&canonical_prefix) {
        return trimmed.to_string();
    }
    let mut lines = vec![canonical_prefix];
    if trimmed.is_empty() {
        lines.push("No model summary text was returned.".to_string());
    } else {
        lines.push(trimmed.to_string());
    }
    lines.join("\n")
}

pub(crate) fn compaction_sequence_order_resists_timestamp_drift_fixture_passes() -> bool {
    compaction_trigger_uses_canonical_history_order_fixture_passes()
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
        return Some(continuation_contract_with_lifecycle_guard_snapshot(
            contract,
            &request.runtime_input.history_items,
        ));
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

    Some(continuation_contract_with_lifecycle_guard_snapshot(
        crate::session::ContinuationContract {
            route: task_route_label(request.state.route).to_string(),
            process_phase: process_phase_label(request.state.process_phase).to_string(),
            active_work_kind: active_work_summary
                .as_ref()
                .map(|_| "typed_continuation".to_string()),
            active_work_summary,
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
            ..crate::session::ContinuationContract::default()
        },
        &request.runtime_input.history_items,
    ))
}

fn continuation_contract_with_lifecycle_guard_snapshot(
    mut contract: crate::session::ContinuationContract,
    history_items: &[HistoryItem],
) -> crate::session::ContinuationContract {
    if let Some((refs, payload, metadata)) =
        latest_lifecycle_guard_snapshot_continuity(history_items)
    {
        if !contract
            .invariant_refs
            .iter()
            .any(|value| value == "LifecycleGuardSnapshot")
        {
            contract
                .invariant_refs
                .push("LifecycleGuardSnapshot".to_string());
        }
        contract.lifecycle_guard_snapshot_refs = refs;
        contract.lifecycle_guard_snapshot_payload = Some(payload);
        contract.lifecycle_guard_snapshot_metadata = metadata;
    }
    contract
}

fn latest_lifecycle_guard_snapshot_continuity(
    history_items: &[HistoryItem],
) -> Option<(
    Vec<String>,
    serde_json::Value,
    std::collections::BTreeMap<String, serde_json::Value>,
)> {
    let canonical_history_items = canonical_history_items_for_compaction(history_items);
    let (item, snapshot) = canonical_history_items
        .as_ref()
        .iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::LifecycleGuard { snapshot } => Some((item, snapshot)),
            _ => None,
        })?;
    let payload = serde_json::to_value(snapshot).ok()?;
    let refs = vec![
        "LifecycleGuardSnapshot".to_string(),
        format!("history_item:{}", item.id),
        format!("turn:{}", item.turn_id),
    ];
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert(
        "counter_count".to_string(),
        serde_json::json!(snapshot.counters.len()),
    );
    metadata.insert(
        "active_flag_count".to_string(),
        serde_json::json!(snapshot.active_flags.len()),
    );
    metadata.insert(
        "scoped_target_count".to_string(),
        serde_json::json!(snapshot.scoped_targets.len()),
    );
    metadata.insert(
        "payload_count".to_string(),
        serde_json::json!(snapshot.payloads.len()),
    );
    Some((refs, payload, metadata))
}

pub(crate) fn compaction_continuity_carries_lifecycle_guard_snapshot_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let mut counters = std::collections::BTreeMap::new();
    counters.insert("rejected_tool:semantic".to_string(), 2);
    let mut payloads = std::collections::BTreeMap::new();
    payloads.insert(
        "invalid_edit_arguments_recovery".to_string(),
        serde_json::json!({"recovery_target":"src/lib.rs"}),
    );
    let guard_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 7,
        created_at_ms: 7,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: crate::protocol::LifecycleGuardSnapshot {
                counters,
                active_flags: vec!["invalid_edit_arguments_recovery".to_string()],
                scoped_targets: vec!["patch_context_mismatch_grounding:src/lib.rs".to_string()],
                payloads,
            },
        },
    };
    let contract = continuation_contract_with_lifecycle_guard_snapshot(
        crate::session::ContinuationContract {
            route: "code".to_string(),
            process_phase: "repair".to_string(),
            invariant_refs: vec!["CompactionContinuity".to_string()],
            ..crate::session::ContinuationContract::default()
        },
        &[guard_item],
    );
    contract
        .invariant_refs
        .contains(&"LifecycleGuardSnapshot".to_string())
        && contract
            .lifecycle_guard_snapshot_refs
            .iter()
            .any(|value| value == "LifecycleGuardSnapshot")
        && contract.lifecycle_guard_snapshot_payload.is_some()
        && contract
            .lifecycle_guard_snapshot_metadata
            .get("payload_count")
            == Some(&serde_json::json!(1))
}

pub(crate) fn compaction_continuity_uses_canonical_history_order_for_lifecycle_guard_snapshot_fixture_passes()
-> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();

    let mut old_payloads = std::collections::BTreeMap::new();
    old_payloads.insert(
        "invalid_edit_arguments_recovery".to_string(),
        serde_json::json!({"recovery_target":"src/old.rs"}),
    );
    let old_guard_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 90,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: crate::protocol::LifecycleGuardSnapshot {
                counters: std::collections::BTreeMap::new(),
                active_flags: vec!["old_recovery".to_string()],
                scoped_targets: vec!["old:src/old.rs".to_string()],
                payloads: old_payloads,
            },
        },
    };

    let mut latest_payloads = std::collections::BTreeMap::new();
    latest_payloads.insert(
        "invalid_edit_arguments_recovery".to_string(),
        serde_json::json!({"recovery_target":"src/latest.rs"}),
    );
    let latest_guard_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 9,
        created_at_ms: 1,
        payload: HistoryItemPayload::LifecycleGuard {
            snapshot: crate::protocol::LifecycleGuardSnapshot {
                counters: std::collections::BTreeMap::new(),
                active_flags: vec!["latest_recovery".to_string()],
                scoped_targets: vec!["latest:src/latest.rs".to_string()],
                payloads: latest_payloads,
            },
        },
    };

    let contract = continuation_contract_with_lifecycle_guard_snapshot(
        crate::session::ContinuationContract {
            route: "code".to_string(),
            process_phase: "repair".to_string(),
            invariant_refs: vec!["CompactionContinuity".to_string()],
            ..crate::session::ContinuationContract::default()
        },
        &[latest_guard_item, old_guard_item],
    );

    contract
        .lifecycle_guard_snapshot_payload
        .as_ref()
        .and_then(|payload| payload.get("payloads"))
        .and_then(|payloads| payloads.get("invalid_edit_arguments_recovery"))
        .and_then(|value| value.get("recovery_target"))
        .and_then(serde_json::Value::as_str)
        == Some("src/latest.rs")
        && contract
            .lifecycle_guard_snapshot_refs
            .iter()
            .any(|value| value == "LifecycleGuardSnapshot")
}

pub(crate) fn compaction_summary_ignores_model_claimed_continuity_fixture_passes() -> bool {
    let deceptive_model_summary =
        "CompactionContinuity was preserved. Continuation focus: all set.";
    let summary = compaction_summary_with_continuity(
        deceptive_model_summary.to_string(),
        9,
        "Route: code\nPhase: repair\nTargets: src/lib.rs",
    );
    summary.starts_with(
        "Summarized history items: 9.\nContinuation invariant: CompactionContinuity.\nContinuation focus:\nRoute: code\nPhase: repair\nTargets: src/lib.rs\nCompacted summary:",
    ) && summary.contains(deceptive_model_summary)
}

pub(crate) fn compaction_lifecycle_guard_sequence_order_resists_timestamp_drift_fixture_passes()
-> bool {
    compaction_continuity_uses_canonical_history_order_for_lifecycle_guard_snapshot_fixture_passes()
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
            model: COMPACTION_FIXTURE_MODEL.to_string(),
            base_url: COMPACTION_FIXTURE_BASE_URL.to_string(),
            access_mode: crate::config::AccessMode::Default,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    #[test]
    fn small_multi_turn_history_does_not_trigger_without_token_pressure() {
        let session_id = SessionId::new();
        let mut items = Vec::new();
        for turn in 0..4 {
            let turn_id = TurnId::new();
            items.push(user_item(session_id, turn_id, turn * 2 + 1));
            items.push(assistant_item(session_id, turn_id, turn * 2 + 2));
        }

        let session = session_record(session_id);
        let pressure_tokens = compaction_trigger_provider_visible_tokens(&session, &items);
        let limit = auto_compact_token_limit(131_072).expect("context window has a limit");

        assert!(pressure_tokens < limit);
    }

    #[test]
    fn provider_visible_pressure_reaches_codex_style_limit() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let items = vec![
            user_item(session_id, turn_id, 1),
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                turn_id,
                sequence_no: 2,
                created_at_ms: 2,
                payload: HistoryItemPayload::Message {
                    message_id: None,
                    role: MessageRole::Assistant,
                    content: vec![ContentPart::Text {
                        text: "x".repeat(4_000),
                    }],
                },
            },
        ];
        let session = session_record(session_id);
        let pressure_tokens = compaction_trigger_provider_visible_tokens(&session, &items);

        assert!(pressure_tokens >= auto_compact_token_limit(1_024).unwrap());
    }

    #[test]
    fn provider_reported_accounting_can_exceed_visible_estimate() {
        let accounting = TokenAccountingState {
            active_context_tokens: 1_200,
            context_window: Some(1_024),
            source: TokenAccountingSource::ProviderReported,
            ..TokenAccountingState::default()
        };

        assert_eq!(compaction_pressure_with_accounting(200, &accounting), 1_200);
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

    #[test]
    fn llm_summary_text_is_wrapped_with_typed_continuity() {
        assert!(llm_summary_text_is_wrapped_with_typed_continuity_fixture_passes());
    }

    #[test]
    fn compaction_summary_ignores_model_claimed_continuity() {
        assert!(compaction_summary_ignores_model_claimed_continuity_fixture_passes());
    }

    #[test]
    fn compaction_continuity_uses_canonical_history_order_for_lifecycle_guard_snapshot() {
        assert!(
            compaction_continuity_uses_canonical_history_order_for_lifecycle_guard_snapshot_fixture_passes()
        );
    }

    #[test]
    fn compaction_sequence_order_resists_timestamp_drift() {
        assert!(compaction_sequence_order_resists_timestamp_drift_fixture_passes());
    }

    #[test]
    fn compaction_lifecycle_guard_sequence_order_resists_timestamp_drift() {
        assert!(compaction_lifecycle_guard_sequence_order_resists_timestamp_drift_fixture_passes());
    }
}
