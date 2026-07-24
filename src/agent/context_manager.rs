use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::context::ContextWindowTokenStatus;
use crate::llm::{ChatRequest, ModelContentPart, ModelMessage, ModelToolCall};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, ModelResponseId,
};
use crate::session::{DurableTurnTerminal, TokenUsage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRevision(String);

impl HistoryRevision {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct ContextManager {
    history_items: Vec<HistoryItem>,
    revision: HistoryRevision,
    append_cursor: Option<i64>,
    canonical_count: usize,
    steer_count: usize,
    agent_communication_count: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveContextTokenState {
    provider_baseline: Option<ProviderTokenBaseline>,
}

#[derive(Debug, Clone)]
struct ProviderTokenBaseline {
    response_id: ModelResponseId,
    total_tokens: u32,
    known_item_ids: HashSet<HistoryItemId>,
}

/// Transient owner used while storage streams one fenced active-history view.
/// Pages move directly into the final ContextManager allocation; callers never
/// need a second whole-history buffer.
#[derive(Debug, Default)]
pub(crate) struct ActiveHistoryContextBuilder {
    history_items: Vec<HistoryItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryChange {
    Unchanged,
    Appended,
    Compacted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDelta {
    pub change: HistoryChange,
    pub steer_item_ids: Vec<HistoryItemId>,
    pub agent_communication_item_ids: Vec<HistoryItemId>,
}

impl ContextManager {
    pub(crate) fn active_history_builder() -> ActiveHistoryContextBuilder {
        ActiveHistoryContextBuilder::default()
    }

    #[cfg(test)]
    pub(crate) fn rehydrate(history_items: Vec<HistoryItem>) -> Self {
        let mut context = Self::from_active_history(Vec::new(), None, 0, 0, 0);
        let _ = context.ingest_committed_delta(history_items, None);
        context
    }

    pub fn from_active_history(
        history_items: Vec<HistoryItem>,
        append_cursor: Option<i64>,
        canonical_count: usize,
        steer_count: usize,
        agent_communication_count: usize,
    ) -> Self {
        let revision = revision_for(&history_items);
        Self {
            history_items,
            revision,
            append_cursor,
            canonical_count,
            steer_count,
            agent_communication_count,
        }
    }

    /// Applies rows read strictly after [`Self::append_cursor`].
    ///
    /// The durable stream remains append-only. A compaction append updates the
    /// active in-memory view by removing its replacement IDs and inserting the
    /// checkpoint at the earliest replaced position. Inputs committed while
    /// compaction was running therefore remain after the checkpoint; replaced
    /// raw content is never reloaded merely to detect this change.
    pub fn ingest_committed_delta(
        &mut self,
        history_items: Vec<HistoryItem>,
        next_cursor: Option<i64>,
    ) -> ContextDelta {
        if history_items.is_empty() {
            return ContextDelta {
                change: HistoryChange::Unchanged,
                steer_item_ids: Vec::new(),
                agent_communication_item_ids: Vec::new(),
            };
        }

        let mut compacted = false;
        let delta_len = history_items.len();
        let mut steer_item_ids = Vec::new();
        let mut agent_communication_item_ids = Vec::new();
        for item in history_items {
            match &item.payload {
                HistoryItemPayload::SteerTurn { .. } => steer_item_ids.push(item.id),
                HistoryItemPayload::InterAgentCommunication { .. } => {
                    agent_communication_item_ids.push(item.id);
                }
                _ => {}
            }
            if let HistoryItemPayload::Compaction {
                replacement_item_ids,
                ..
            } = &item.payload
            {
                compacted = true;
                let replaced = replacement_item_ids.iter().copied().collect::<HashSet<_>>();
                let insertion_index = self
                    .history_items
                    .iter()
                    .position(|existing| replaced.contains(&existing.id))
                    .unwrap_or(self.history_items.len());
                self.history_items
                    .retain(|existing| !replaced.contains(&existing.id));
                let insertion_index = insertion_index.min(self.history_items.len());
                self.history_items.insert(insertion_index, item);
            } else {
                self.history_items.push(item);
            }
        }
        // Canonical count follows append identities rather than active-view
        // length; compaction may shrink the latter.
        self.canonical_count = self.canonical_count.saturating_add(delta_len);
        self.steer_count = self.steer_count.saturating_add(steer_item_ids.len());
        self.agent_communication_count = self
            .agent_communication_count
            .saturating_add(agent_communication_item_ids.len());
        self.append_cursor = next_cursor.or(self.append_cursor);
        self.revision = revision_for(&self.history_items);
        ContextDelta {
            change: if compacted {
                HistoryChange::Compacted
            } else {
                HistoryChange::Appended
            },
            steer_item_ids,
            agent_communication_item_ids,
        }
    }

    pub fn revision(&self) -> &HistoryRevision {
        &self.revision
    }

    pub fn append_cursor(&self) -> Option<i64> {
        self.append_cursor
    }

    pub fn canonical_count(&self) -> usize {
        self.canonical_count
    }

    pub fn steer_count(&self) -> usize {
        self.steer_count
    }

    pub fn agent_communication_count(&self) -> usize {
        self.agent_communication_count
    }

    pub fn history_items(&self) -> &[HistoryItem] {
        &self.history_items
    }

    pub fn has_model_context(&self) -> bool {
        self.history_items.iter().any(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::InterAgentCommunication { .. }
                    | HistoryItemPayload::Compaction { .. }
            )
        })
    }

    pub fn model_messages(&self, supports_images: bool) -> Vec<ModelMessage> {
        project_model_messages(&self.history_items, supports_images)
    }

    pub fn model_messages_for_items(
        &self,
        item_ids: &[HistoryItemId],
        supports_images: bool,
    ) -> Vec<ModelMessage> {
        let selected = item_ids.iter().copied().collect::<HashSet<_>>();
        let items = self
            .history_items
            .iter()
            .filter(|item| selected.contains(&item.id))
            .cloned()
            .collect::<Vec<_>>();
        project_model_messages(&items, supports_images)
    }

    pub fn active_item_ids(&self) -> Vec<HistoryItemId> {
        let replaced = crate::protocol::compacted_history_item_ids(&self.history_items);
        self.history_items
            .iter()
            .filter(|item| !replaced.contains(&item.id))
            .map(|item| item.id)
            .collect()
    }

    fn active_item_ids_through_model_response(
        &self,
        response_id: ModelResponseId,
    ) -> Option<HashSet<HistoryItemId>> {
        let active = self.active_history_items();
        if latest_model_response_id(&active) != Some(response_id) {
            return None;
        }
        let last_response_index = active.iter().rposition(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::AssistantMessage {
                    response_id: item_response_id,
                    ..
                } | HistoryItemPayload::ToolCall {
                    response_id: item_response_id,
                    ..
                } if item_response_id == response_id
            )
        })?;
        Some(
            active[..=last_response_index]
                .iter()
                .map(|item| item.id)
                .collect(),
        )
    }

    fn active_history_items(&self) -> Vec<&HistoryItem> {
        let replaced = crate::protocol::compacted_history_item_ids(&self.history_items);
        self.history_items
            .iter()
            .filter(|item| !replaced.contains(&item.id))
            .collect()
    }

    fn local_messages_after_provider_baseline(
        &self,
        baseline: &ProviderTokenBaseline,
        supports_images: bool,
    ) -> Option<Vec<ModelMessage>> {
        let active = self.active_history_items();
        if latest_model_response_id(&active) != Some(baseline.response_id) {
            return None;
        }

        let tool_calls = active
            .iter()
            .filter_map(|item| match &item.payload {
                HistoryItemPayload::ToolCall {
                    call_id,
                    model_call_id,
                    tool_name,
                    ..
                } => Some((
                    *call_id,
                    (
                        if model_call_id.trim().is_empty() {
                            call_id.to_string()
                        } else {
                            model_call_id.clone()
                        },
                        tool_name.clone(),
                    ),
                )),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut messages = Vec::new();
        for item in active {
            if baseline.known_item_ids.contains(&item.id) {
                continue;
            }
            match &item.payload {
                HistoryItemPayload::UserTurn { content, .. }
                | HistoryItemPayload::SteerTurn { content, .. } => {
                    messages.push(user_message_from_content(content, supports_images));
                }
                HistoryItemPayload::InterAgentCommunication { communication } => {
                    messages.push(inter_agent_input_message(communication));
                }
                HistoryItemPayload::AssistantMessage { response_id, .. }
                | HistoryItemPayload::ToolCall { response_id, .. }
                    if *response_id == baseline.response_id => {}
                HistoryItemPayload::AssistantMessage { .. }
                | HistoryItemPayload::ToolCall { .. }
                | HistoryItemPayload::Compaction { .. } => return None,
                HistoryItemPayload::ToolOutput {
                    call_id,
                    output_text,
                    metadata,
                    ..
                } => {
                    let (model_call_id, tool_name) = tool_calls.get(call_id)?;
                    messages.push(ModelMessage::Tool {
                        call_id: model_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: model_visible_tool_output(tool_name, output_text, metadata),
                        metadata: serde_json::Value::Null,
                    });
                }
                HistoryItemPayload::Error { message } => {
                    messages.push(ModelMessage::Assistant {
                        content: format!("Previous run ended with an error: {message}"),
                    });
                }
                HistoryItemPayload::SubAgentActivity { .. }
                | HistoryItemPayload::CollaborationModeInstruction { .. }
                | HistoryItemPayload::RequestDiagnostics { .. }
                | HistoryItemPayload::WorldState { .. }
                | HistoryItemPayload::ApprovalDecision { .. }
                | HistoryItemPayload::FileChange { .. } => {}
            }
        }
        Some(messages)
    }

    pub fn semantic_compaction_units(&self) -> Vec<Vec<HistoryItemId>> {
        let replaced = crate::protocol::compacted_history_item_ids(&self.history_items);
        let active = self
            .history_items
            .iter()
            .enumerate()
            .filter(|(_, item)| !replaced.contains(&item.id))
            .collect::<Vec<_>>();

        let mut call_response = HashMap::new();
        let mut response_items =
            HashMap::<crate::protocol::ModelResponseId, Vec<(usize, HistoryItemId)>>::new();
        let mut response_calls =
            HashMap::<crate::protocol::ModelResponseId, Vec<crate::session::ToolCallId>>::new();
        let mut output_calls = HashSet::new();

        for (index, item) in &active {
            match &item.payload {
                HistoryItemPayload::AssistantMessage { response_id, .. } => {
                    response_items
                        .entry(*response_id)
                        .or_default()
                        .push((*index, item.id));
                }
                HistoryItemPayload::ToolCall {
                    call_id,
                    response_id,
                    ..
                } => {
                    call_response.insert(*call_id, *response_id);
                    response_calls
                        .entry(*response_id)
                        .or_default()
                        .push(*call_id);
                    response_items
                        .entry(*response_id)
                        .or_default()
                        .push((*index, item.id));
                }
                HistoryItemPayload::ToolOutput { call_id, .. } => {
                    output_calls.insert(*call_id);
                }
                _ => {}
            }
        }
        for (index, item) in &active {
            let HistoryItemPayload::ToolOutput { call_id, .. } = &item.payload else {
                continue;
            };
            if let Some(response_id) = call_response.get(call_id) {
                response_items
                    .entry(*response_id)
                    .or_default()
                    .push((*index, item.id));
            }
        }

        let first_unsettled_response_index = response_items
            .iter()
            .filter(|(response_id, _)| {
                response_calls.get(response_id).is_some_and(|calls| {
                    calls.iter().any(|call_id| !output_calls.contains(call_id))
                })
            })
            .filter_map(|(_, items)| items.iter().map(|(index, _)| *index).min())
            .min();
        if first_unsettled_response_index.is_some() {
            // Keep the compaction source stable and complete. The response becomes
            // one semantic unit only after every call has its matching output.
            return Vec::new();
        }

        let mut units = Vec::<(usize, Vec<HistoryItemId>)>::new();
        for (response_id, mut items) in response_items {
            if response_calls
                .get(&response_id)
                .is_some_and(|calls| calls.iter().any(|call_id| !output_calls.contains(call_id)))
            {
                continue;
            }
            items.sort_by_key(|(index, _)| *index);
            if let Some(first_index) = items.first().map(|(index, _)| *index) {
                units.push((
                    first_index,
                    items.into_iter().map(|(_, item_id)| item_id).collect(),
                ));
            }
        }
        for (index, item) in active {
            if matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::SteerTurn { .. }
                    | HistoryItemPayload::InterAgentCommunication { .. }
                    | HistoryItemPayload::Compaction { .. }
                    | HistoryItemPayload::Error { .. }
            ) {
                units.push((index, vec![item.id]));
            }
        }
        units.sort_by_key(|(index, _)| *index);
        units.into_iter().map(|(_, item_ids)| item_ids).collect()
    }

    pub fn compaction_user_messages_for_items(&self, item_ids: &[HistoryItemId]) -> Vec<String> {
        let selected = item_ids.iter().copied().collect::<HashSet<_>>();
        let replaced = crate::protocol::compacted_history_item_ids(&self.history_items);
        let mut seen = HashSet::new();
        self.history_items
            .iter()
            .filter(|item| {
                selected.contains(&item.id) && !replaced.contains(&item.id) && seen.insert(item.id)
            })
            .flat_map(|item| match &item.payload {
                HistoryItemPayload::UserTurn { content, .. }
                | HistoryItemPayload::SteerTurn { content, .. } => {
                    let text = content_text(content);
                    if text.trim().is_empty() {
                        Vec::new()
                    } else {
                        vec![text]
                    }
                }
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.trigger_turn =>
                {
                    let text = inter_agent_input_text(communication);
                    if text.trim().is_empty() {
                        Vec::new()
                    } else {
                        vec![text]
                    }
                }
                HistoryItemPayload::Compaction {
                    layout,
                    preserved_user_messages,
                    ..
                } if layout.appends_checkpoint() => preserved_user_messages.clone(),
                _ => Vec::new(),
            })
            .collect()
    }
}

impl ActiveContextTokenState {
    pub(crate) fn rehydrate(
        context: &ContextManager,
        terminal: Option<&DurableTurnTerminal>,
    ) -> Self {
        let provider_baseline = terminal.and_then(|terminal| {
            if !matches!(
                terminal.outcome,
                crate::protocol::TurnTerminalOutcome::Completed
            ) {
                return None;
            }
            let response_id = terminal.final_response_id?;
            let usage = terminal.metrics.token_usage.as_ref()?;
            let known_item_ids = context.active_item_ids_through_model_response(response_id)?;
            Some(ProviderTokenBaseline {
                response_id,
                total_tokens: usage.total_tokens,
                known_item_ids,
            })
        });
        Self { provider_baseline }
    }

    pub(crate) fn status_for_request(
        &self,
        context: &ContextManager,
        request: &ChatRequest,
        supports_images: bool,
        overflow_margin_tokens: usize,
    ) -> ContextWindowTokenStatus {
        if let Some(baseline) = &self.provider_baseline
            && let Some(local_messages) =
                context.local_messages_after_provider_baseline(baseline, supports_images)
        {
            return ContextWindowTokenStatus::from_provider_usage(
                request,
                overflow_margin_tokens,
                baseline.total_tokens,
                &local_messages,
            );
        }
        ContextWindowTokenStatus::for_request(request, overflow_margin_tokens)
    }

    pub(crate) fn record_provider_response(
        &mut self,
        response_id: ModelResponseId,
        usage: Option<&TokenUsage>,
        known_item_ids: Vec<HistoryItemId>,
    ) {
        self.provider_baseline = usage.map(|usage| ProviderTokenBaseline {
            response_id,
            total_tokens: usage.total_tokens,
            known_item_ids: known_item_ids.into_iter().collect(),
        });
    }

    pub(crate) fn reset_after_compaction(&mut self) {
        self.provider_baseline = None;
    }
}

fn latest_model_response_id(active_items: &[&HistoryItem]) -> Option<ModelResponseId> {
    active_items
        .iter()
        .rev()
        .find_map(|item| match item.payload {
            HistoryItemPayload::AssistantMessage { response_id, .. }
            | HistoryItemPayload::ToolCall { response_id, .. } => Some(response_id),
            _ => None,
        })
}

impl ActiveHistoryContextBuilder {
    pub(crate) fn ingest_page(&mut self, history_items: Vec<HistoryItem>) {
        self.history_items.extend(history_items);
    }

    pub(crate) fn finish(
        self,
        append_cursor: Option<i64>,
        canonical_count: usize,
        steer_count: usize,
        agent_communication_count: usize,
    ) -> ContextManager {
        ContextManager::from_active_history(
            self.history_items,
            append_cursor,
            canonical_count,
            steer_count,
            agent_communication_count,
        )
    }
}

fn revision_for(items: &[HistoryItem]) -> HistoryRevision {
    let mut hash = Sha256::new();
    for item in items {
        hash.update(item.id.to_string().as_bytes());
        hash.update(item.scope.as_str().as_bytes());
        if let Some(turn_id) = item.turn_id() {
            hash.update(turn_id.to_string().as_bytes());
        }
        hash.update(item.sequence_no.to_le_bytes());
        if let Ok(payload) = serde_json::to_vec(&item.payload) {
            hash.update(payload);
        }
    }
    HistoryRevision(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn project_model_messages(
    history_items: &[HistoryItem],
    supports_images: bool,
) -> Vec<ModelMessage> {
    let mut projected = Vec::<(usize, usize, ModelMessage)>::new();
    let index_by_id = history_items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, index))
        .collect::<HashMap<_, _>>();
    let replaced_ids = crate::protocol::compacted_history_item_ids(history_items);
    let output_call_ids = history_items
        .iter()
        .filter(|item| !replaced_ids.contains(&item.id))
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::ToolOutput { call_id, .. } => Some(*call_id),
            _ => None,
        })
        .collect::<HashSet<_>>();

    let mut calls_by_response = HashMap::<
        crate::protocol::ModelResponseId,
        Vec<(usize, crate::session::ToolCallId, String, String, String)>,
    >::new();
    let mut response_first_index = HashMap::<crate::protocol::ModelResponseId, usize>::new();
    let mut assistant_content_by_response =
        HashMap::<crate::protocol::ModelResponseId, String>::new();
    for (index, item) in history_items.iter().enumerate() {
        if replaced_ids.contains(&item.id) {
            continue;
        }
        match &item.payload {
            HistoryItemPayload::AssistantMessage {
                response_id,
                content,
                ..
            } => {
                response_first_index
                    .entry(*response_id)
                    .and_modify(|first| *first = (*first).min(index))
                    .or_insert(index);
                assistant_content_by_response
                    .entry(*response_id)
                    .and_modify(|text| {
                        let next = content_text(content);
                        if !next.is_empty() {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&next);
                        }
                    })
                    .or_insert_with(|| content_text(content));
            }
            HistoryItemPayload::ToolCall {
                call_id,
                response_id,
                model_call_id,
                tool_name,
                arguments_json,
            } if output_call_ids.contains(call_id) => {
                let replay_call_id = if model_call_id.trim().is_empty() {
                    call_id.to_string()
                } else {
                    model_call_id.clone()
                };
                response_first_index
                    .entry(*response_id)
                    .and_modify(|first| *first = (*first).min(index))
                    .or_insert(index);
                calls_by_response.entry(*response_id).or_default().push((
                    index,
                    *call_id,
                    replay_call_id,
                    tool_name.clone(),
                    arguments_json.clone(),
                ));
            }
            _ => {}
        }
    }

    let mut tool_names_by_call = HashMap::new();
    for (response_id, calls) in &calls_by_response {
        let Some(first_index) = response_first_index.get(response_id).copied() else {
            continue;
        };
        let mut ordered = calls.clone();
        ordered.sort_by_key(|(index, ..)| *index);
        let tool_calls = ordered
            .into_iter()
            .map(|(_, call_id, replay_call_id, tool_name, arguments_json)| {
                tool_names_by_call.insert(
                    call_id.to_string(),
                    (replay_call_id.clone(), tool_name.clone()),
                );
                ModelToolCall {
                    call_id: replay_call_id,
                    tool_name,
                    arguments_json,
                }
            })
            .collect::<Vec<_>>();
        let content = assistant_content_by_response
            .get(response_id)
            .cloned()
            .filter(|text| !text.is_empty());
        projected.push((
            first_index,
            1,
            ModelMessage::AssistantToolCalls {
                content,
                tool_calls,
            },
        ));
    }

    for (index, item) in history_items.iter().enumerate() {
        if replaced_ids.contains(&item.id) {
            continue;
        }
        match &item.payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::SteerTurn { content, .. } => projected.push((
                index,
                1,
                user_message_from_content(content, supports_images),
            )),
            HistoryItemPayload::AssistantMessage {
                response_id,
                content,
                ..
            } => {
                if calls_by_response.contains_key(response_id) {
                    continue;
                }
                projected.push((
                    index,
                    1,
                    ModelMessage::Assistant {
                        content: content_text(content),
                    },
                ));
            }
            HistoryItemPayload::InterAgentCommunication { communication } => {
                projected.push((index, 1, inter_agent_input_message(communication)))
            }
            HistoryItemPayload::ToolCall { .. } => {}
            HistoryItemPayload::ToolOutput {
                call_id,
                output_text,
                metadata,
                ..
            } => {
                let call_id_text = call_id.to_string();
                if let Some((model_call_id, tool_name)) =
                    tool_names_by_call.get(&call_id_text).cloned()
                {
                    let result = model_visible_tool_output(&tool_name, output_text, metadata);
                    projected.push((
                        index,
                        1,
                        ModelMessage::Tool {
                            call_id: model_call_id,
                            tool_name,
                            result,
                            metadata: serde_json::Value::Null,
                        },
                    ));
                }
            }
            HistoryItemPayload::Compaction {
                layout,
                preserved_user_messages,
                summary,
                replacement_item_ids,
                ..
            } => {
                if layout.appends_checkpoint() {
                    for (priority, message) in preserved_user_messages.iter().enumerate() {
                        projected.push((
                            index,
                            priority,
                            ModelMessage::User {
                                content: message.clone(),
                            },
                        ));
                    }
                    projected.push((
                        index,
                        preserved_user_messages.len(),
                        semantic_compaction_message(summary),
                    ));
                } else {
                    let insertion_index = replacement_item_ids
                        .iter()
                        .filter_map(|id| index_by_id.get(id).copied())
                        .min()
                        .unwrap_or(index);
                    projected.push((insertion_index, 0, semantic_compaction_message(summary)));
                }
            }
            HistoryItemPayload::Error { message, .. } => projected.push((
                index,
                1,
                ModelMessage::Assistant {
                    content: format!("Previous run ended with an error: {message}"),
                },
            )),
            _ => {}
        }
    }
    projected.sort_by_key(|(index, priority, _)| (*index, *priority));
    projected
        .into_iter()
        .map(|(_, _, message)| message)
        .collect()
}

fn model_visible_tool_output(
    tool_name: &str,
    output_text: &str,
    metadata: &serde_json::Value,
) -> String {
    let preview_truncated = metadata
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if tool_name == "read" {
        if preview_truncated {
            return format!("{output_text}\n\nWarning: truncated output.");
        }
        let total_lines = metadata
            .get("total_lines")
            .and_then(serde_json::Value::as_u64);
        let end_line = metadata.get("end_line").and_then(serde_json::Value::as_u64);
        if let (Some(total_lines), Some(end_line)) = (total_lines, end_line)
            && end_line < total_lines
        {
            let next_offset = end_line.saturating_add(1);
            return format!(
                "{output_text}\n\nWarning: truncated output.\nTotal output lines: {total_lines}. Continue with `read` using `offset`: {next_offset}."
            );
        }
        return output_text.to_string();
    }
    if matches!(tool_name, "list" | "glob" | "grep" | "inspect_directory") && preview_truncated {
        return format!("{output_text}\n\nWarning: truncated output.");
    }
    output_text.to_string()
}

pub(super) fn semantic_compaction_message(summary: &str) -> ModelMessage {
    ModelMessage::User {
        content: format!(
            "{}\n{}",
            include_str!("../../assets/prompts/compaction_summary_prefix.md").trim(),
            summary.trim()
        ),
    }
}

pub(super) fn is_semantic_compaction_message(message: &ModelMessage) -> bool {
    let prefix = include_str!("../../assets/prompts/compaction_summary_prefix.md").trim();
    matches!(
        message,
        ModelMessage::User { content }
            if content
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('\n'))
    )
}

fn user_message_from_content(content: &[ContentPart], supports_images: bool) -> ModelMessage {
    let has_supported_image = supports_images
        && content
            .iter()
            .any(|part| matches!(part, ContentPart::Image { .. }));
    if !has_supported_image {
        let mut text = content_text(content);
        let omitted = content
            .iter()
            .filter(|part| matches!(part, ContentPart::Image { .. }))
            .count();
        if omitted > 0 {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "[{omitted} image input(s) omitted because the selected model does not support images]"
            ));
        }
        return ModelMessage::User { content: text };
    }
    ModelMessage::UserParts {
        parts: content
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => ModelContentPart::Text { text: text.clone() },
                ContentPart::Image { image } => ModelContentPart::Image {
                    mime_type: image.mime_type.clone(),
                    data_base64: image.data_base64.clone(),
                },
            })
            .collect(),
    }
}

fn inter_agent_input_message(
    communication: &crate::protocol::InterAgentCommunication,
) -> ModelMessage {
    ModelMessage::Agent {
        content: inter_agent_input_text(communication),
    }
}

fn inter_agent_input_text(communication: &crate::protocol::InterAgentCommunication) -> String {
    if crate::protocol::is_rendered_inter_agent_message(communication) {
        return communication.content.clone();
    }
    crate::protocol::render_inter_agent_message(
        if communication.trigger_turn {
            crate::protocol::InterAgentMessageType::NewTask
        } else {
            crate::protocol::InterAgentMessageType::Message
        },
        &communication.recipient,
        &communication.author,
        &communication.content,
    )
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::context::ActiveContextTokenSource;
    use crate::llm::{ModelCapabilities, ModelProfile};
    use crate::protocol::{HistoryScope, ModelResponseId, ToolLifecycleStatus, TurnId};
    use crate::session::{ImagePart, RunMetrics, SessionId, ToolCallId};

    fn user_item(text: &str) -> HistoryItem {
        let session_id = SessionId::new();
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        }
    }

    fn token_status_request(messages: Vec<ModelMessage>) -> ChatRequest {
        let model = ModelProfile {
            name: "test".to_string(),
            context_window: 32_768,
            max_output_tokens: 512,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        };
        let provider = ProviderTarget::new(
            "http://localhost",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 1,
                stream_idle_timeout_ms: 1,
                connect_timeout_ms: 1,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        ChatRequest::new(
            provider,
            model,
            "runtime prompt".to_string(),
            messages,
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        )
    }

    fn provider_tool_round() -> (ContextManager, HistoryItemId, ModelResponseId) {
        let user = user_item("inspect the file");
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let tool_call = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: user.scope,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                response_id,
                model_call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                arguments_json: r#"{"path":"task.md"}"#.to_string(),
            },
        };
        let tool_output = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: user.scope,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "read".to_string(),
                output_text: "local tool output after provider response".to_string(),
                metadata: serde_json::Value::Null,
                success: Some(true),
            },
        };
        let user_id = user.id;
        (
            ContextManager::rehydrate(vec![user, tool_call, tool_output]),
            user_id,
            response_id,
        )
    }

    fn usage(total_tokens: u32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: total_tokens,
            completion_tokens: 0,
            total_tokens,
            reasoning_tokens: None,
        }
    }

    #[test]
    fn provider_usage_drives_the_next_step_with_only_local_suffix_estimated() {
        let (context, user_id, response_id) = provider_tool_round();
        let request = token_status_request(context.model_messages(false));
        let mut state = ActiveContextTokenState::default();
        state.record_provider_response(response_id, Some(&usage(1_000)), vec![user_id]);

        let status = state.status_for_request(&context, &request, false, 128);

        assert_eq!(
            status.source,
            ActiveContextTokenSource::ProviderUsageWithLocalEstimate
        );
        assert!(status.active_context_tokens > 1_000);
        assert!(
            status.active_context_tokens
                < ContextWindowTokenStatus::for_request(&request, 128).active_context_tokens
                    + 1_000
        );
    }

    #[test]
    fn missing_provider_usage_falls_back_to_the_full_prepared_request_estimate() {
        let (context, user_id, response_id) = provider_tool_round();
        let request = token_status_request(context.model_messages(false));
        let mut state = ActiveContextTokenState::default();
        state.record_provider_response(response_id, None, vec![user_id]);

        let status = state.status_for_request(&context, &request, false, 128);
        let expected = ContextWindowTokenStatus::for_request(&request, 128);

        assert_eq!(
            status.source,
            ActiveContextTokenSource::FullPreparedRequestEstimate
        );
        assert_eq!(status, expected);
    }

    #[test]
    fn compaction_reset_forces_a_full_local_recompute() {
        let (context, user_id, response_id) = provider_tool_round();
        let request = token_status_request(context.model_messages(false));
        let mut state = ActiveContextTokenState::default();
        state.record_provider_response(response_id, Some(&usage(1_000)), vec![user_id]);
        assert_eq!(
            state
                .status_for_request(&context, &request, false, 128)
                .source,
            ActiveContextTokenSource::ProviderUsageWithLocalEstimate
        );

        state.reset_after_compaction();

        assert_eq!(
            state
                .status_for_request(&context, &request, false, 128)
                .source,
            ActiveContextTokenSource::FullPreparedRequestEstimate
        );
    }

    #[test]
    fn durable_terminal_rehydrates_only_a_matching_latest_model_response() {
        let first_user = user_item("first turn");
        let response_id = ModelResponseId::new();
        let response = HistoryItem {
            id: HistoryItemId::new(),
            session_id: first_user.session_id,
            scope: first_user.scope,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::AssistantMessage {
                response_id,
                content: vec![ContentPart::Text {
                    text: "done".to_string(),
                }],
            },
        };
        let mut next_user = user_item("second turn local input");
        next_user.session_id = first_user.session_id;
        next_user.sequence_no = 3;
        next_user.created_at_ms = 3;
        let context = ContextManager::rehydrate(vec![first_user, response, next_user]);
        let request = token_status_request(context.model_messages(false));
        let terminal = DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: Some(response_id),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: RunMetrics {
                token_usage: Some(usage(700)),
                ..RunMetrics::default()
            },
        };

        let state = ActiveContextTokenState::rehydrate(&context, Some(&terminal));
        let status = state.status_for_request(&context, &request, false, 128);

        assert_eq!(
            status.source,
            ActiveContextTokenSource::ProviderUsageWithLocalEstimate
        );
        assert!(status.active_context_tokens > 700);

        let mut failed = terminal.clone();
        failed.outcome = crate::protocol::TurnTerminalOutcome::Failed {
            error: "provider request failed after reporting usage".to_string(),
        };
        let failed_fallback = ActiveContextTokenState::rehydrate(&context, Some(&failed))
            .status_for_request(&context, &request, false, 128);
        assert_eq!(
            failed_fallback.source,
            ActiveContextTokenSource::FullPreparedRequestEstimate
        );

        let mut mismatched = terminal;
        mismatched.final_response_id = Some(ModelResponseId::new());
        let fallback = ActiveContextTokenState::rehydrate(&context, Some(&mismatched))
            .status_for_request(&context, &request, false, 128);
        assert_eq!(
            fallback.source,
            ActiveContextTokenSource::FullPreparedRequestEstimate
        );
    }

    #[test]
    fn paginated_tool_output_exposes_a_durable_truncation_warning() {
        let projected = model_visible_tool_output(
            "grep",
            "first page",
            &serde_json::json!({
                "truncated": true,
                "continuation": "next\npage"
            }),
        );

        assert_eq!(projected, "first page\n\nWarning: truncated output.");
    }

    #[test]
    fn read_output_uses_a_repeatable_offset_instead_of_an_opaque_cursor() {
        assert_eq!(
            model_visible_tool_output(
                "read",
                "first 2,000 lines",
                &serde_json::json!({
                    "truncated": false,
                    "end_line": 2_000,
                    "total_lines": 2_400
                })
            ),
            "first 2,000 lines\n\nWarning: truncated output.\nTotal output lines: 2400. Continue with `read` using `offset`: 2001."
        );
        assert_eq!(
            model_visible_tool_output(
                "read",
                "preview",
                &serde_json::json!({
                    "truncated": true,
                    "end_line": 100,
                    "total_lines": 100
                })
            ),
            "preview\n\nWarning: truncated output."
        );
    }

    #[test]
    fn complete_and_unrelated_tool_outputs_are_unchanged() {
        let metadata = serde_json::json!({
            "truncated": true,
            "continuation": "next-page"
        });

        assert_eq!(
            model_visible_tool_output("shell", "shell preview", &metadata),
            "shell preview"
        );
        assert_eq!(
            model_visible_tool_output(
                "list",
                "complete list",
                &serde_json::json!({"truncated": false})
            ),
            "complete list"
        );
    }

    #[test]
    fn canonical_paginated_tool_output_projects_only_a_stable_warning() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let projected = ContextManager::rehydrate(vec![
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 0,
                created_at_ms: 1,
                payload: HistoryItemPayload::ToolCall {
                    call_id,
                    response_id,
                    model_call_id: "provider-list-call".to_string(),
                    tool_name: "list".to_string(),
                    arguments_json: serde_json::json!({"path": "."}).to_string(),
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: 2,
                payload: HistoryItemPayload::ToolOutput {
                    call_id,
                    status: ToolLifecycleStatus::Completed,
                    title: "list".to_string(),
                    output_text: "first page".to_string(),
                    metadata: serde_json::json!({
                        "truncated": true,
                        "continuation": "list-next-page"
                    }),
                    success: Some(true),
                },
            },
        ])
        .model_messages(false);

        assert!(matches!(
            projected.as_slice(),
            [
                ModelMessage::AssistantToolCalls { tool_calls, .. },
                ModelMessage::Tool { result, .. },
            ] if matches!(tool_calls.as_slice(), [ModelToolCall { tool_name, .. }] if tool_name == "list")
                && result.contains("first page")
                && result.contains("Warning: truncated output")
                && !result.contains("list-next-page")
                && !result.contains("cursor")
        ));
    }

    #[test]
    fn revision_changes_only_with_canonical_history() {
        let first = user_item("one");
        let mut context = ContextManager::from_active_history(vec![first], Some(1), 1, 0, 0);
        let revision = context.revision().clone();
        assert_eq!(
            context.ingest_committed_delta(Vec::new(), None).change,
            HistoryChange::Unchanged,
        );
        assert_eq!(context.revision(), &revision);
        let second = user_item("two");
        assert_eq!(
            context.ingest_committed_delta(vec![second], Some(2)).change,
            HistoryChange::Appended,
        );
        assert_ne!(context.revision(), &revision);
        assert_eq!(context.append_cursor(), Some(2));
        assert_eq!(context.canonical_count(), 2);
    }

    #[test]
    fn active_history_builder_moves_bounded_pages_into_one_context_owner() {
        let first = user_item("first page");
        let mut second = user_item("second page");
        second.session_id = first.session_id;
        second.scope = first.scope;
        second.sequence_no = 1;
        let expected_revision = revision_for(&[first.clone(), second.clone()]);
        let mut builder = ContextManager::active_history_builder();

        builder.ingest_page(vec![first.clone()]);
        builder.ingest_page(vec![second.clone()]);
        let context = builder.finish(Some(9), 7, 2, 1);

        assert_eq!(context.revision(), &expected_revision);
        assert_eq!(context.append_cursor(), Some(9));
        assert_eq!(context.canonical_count(), 7);
        assert_eq!(context.steer_count(), 2);
        assert_eq!(context.agent_communication_count(), 1);
        assert_eq!(
            context
                .history_items()
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
    }

    #[test]
    fn compaction_delta_keeps_late_input_after_checkpoint_without_full_rehydrate() {
        let first = user_item("first");
        let mut second = user_item("second");
        second.session_id = first.session_id;
        second.scope = first.scope;
        second.sequence_no = 2;
        let mut tail = user_item("tail");
        tail.session_id = first.session_id;
        tail.scope = first.scope;
        tail.sequence_no = 3;
        let mut context = ContextManager::from_active_history(
            vec![first.clone(), second.clone(), tail.clone()],
            Some(3),
            3,
            0,
            0,
        );
        let summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id: first.session_id,
            scope: first.scope,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: vec!["first".to_string(), "second".to_string()],
                summary: "first and second summarized".to_string(),
                replacement_item_ids: vec![first.id, second.id],
            },
        };

        let delta = context.ingest_committed_delta(vec![summary.clone()], Some(4));

        assert_eq!(delta.change, HistoryChange::Compacted);
        assert_eq!(context.append_cursor(), Some(4));
        assert_eq!(context.canonical_count(), 4);
        assert_eq!(
            context
                .history_items()
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![summary.id, tail.id]
        );
        assert!(matches!(
            context.model_messages(false).as_slice(),
            [
                ModelMessage::User { content: first_text },
                ModelMessage::User { content: second_text },
                ModelMessage::User { content },
                ModelMessage::User { content: tail_text },
            ] if first_text == "first"
                && second_text == "second"
                && content.contains("first and second summarized")
                && tail_text == "tail"
        ));
    }

    #[test]
    fn legacy_compaction_keeps_summary_at_the_replaced_prefix_position() {
        let first = user_item("first");
        let mut tail = user_item("tail");
        tail.session_id = first.session_id;
        tail.scope = first.scope;
        tail.sequence_no = 2;
        let mut context = ContextManager::from_active_history(
            vec![first.clone(), tail.clone()],
            Some(2),
            2,
            0,
            0,
        );
        let summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id: first.session_id,
            scope: first.scope,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::LegacyPrefix,
                preserved_user_messages: Vec::new(),
                summary: "legacy summary".to_string(),
                replacement_item_ids: vec![first.id],
            },
        };

        let _ = context.ingest_committed_delta(vec![summary.clone()], Some(3));

        assert_eq!(
            context
                .history_items()
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![summary.id, tail.id]
        );
        assert!(matches!(
            context.model_messages(false).as_slice(),
            [ModelMessage::User { content }, ModelMessage::User { content: tail_text }]
                if content.contains("legacy summary") && tail_text == "tail"
        ));
    }

    #[test]
    fn compaction_retains_real_user_input_and_replaces_settled_tool_evidence() {
        let old = user_item("old request detail");
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let retained = [
            HistoryItem {
                id: HistoryItemId::new(),
                session_id: old.session_id,
                scope: old.scope,
                sequence_no: 2,
                created_at_ms: 2,
                payload: HistoryItemPayload::AssistantMessage {
                    response_id,
                    content: vec![ContentPart::Text {
                        text: "I will inspect the retained file.".to_string(),
                    }],
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id: old.session_id,
                scope: old.scope,
                sequence_no: 3,
                created_at_ms: 3,
                payload: HistoryItemPayload::ToolCall {
                    call_id,
                    response_id,
                    model_call_id: "call-retained".to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: serde_json::json!({"path": "retained.txt"}).to_string(),
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id: old.session_id,
                scope: old.scope,
                sequence_no: 4,
                created_at_ms: 4,
                payload: HistoryItemPayload::ToolOutput {
                    call_id,
                    status: ToolLifecycleStatus::Completed,
                    title: "read".to_string(),
                    output_text: "retained contents".to_string(),
                    metadata: serde_json::Value::Null,
                    success: Some(true),
                },
            },
        ];
        let mut active = vec![old.clone()];
        active.extend(retained.clone());
        let mut context = ContextManager::from_active_history(active, Some(4), 4, 0, 0);
        let compaction = HistoryItem {
            id: HistoryItemId::new(),
            session_id: old.session_id,
            scope: old.scope,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: Vec::new(),
                summary: "Continue the retained inspection.".to_string(),
                replacement_item_ids: retained.iter().map(|item| item.id).collect(),
            },
        };

        let delta = context.ingest_committed_delta(vec![compaction], Some(5));
        let projected = context.model_messages(false);

        assert_eq!(delta.change, HistoryChange::Compacted);
        assert!(matches!(
            projected.as_slice(),
            [
                ModelMessage::User { content: original },
                ModelMessage::User { content: summary },
            ] if original == "old request detail"
                && summary.contains("Continue the retained inspection.")
        ));
    }

    #[test]
    fn repeated_compaction_keeps_real_users_and_replaces_the_prior_summary() {
        let first_user = user_item("original task");
        let mut first_assistant = user_item("unused");
        first_assistant.session_id = first_user.session_id;
        first_assistant.scope = first_user.scope;
        first_assistant.sequence_no = 2;
        first_assistant.payload = HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: "first work".to_string(),
            }],
        };
        let mut context = ContextManager::from_active_history(
            vec![first_user.clone(), first_assistant.clone()],
            Some(2),
            2,
            0,
            0,
        );
        let first_summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id: first_user.session_id,
            scope: first_user.scope,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: vec!["original task".to_string()],
                summary: "first checkpoint".to_string(),
                replacement_item_ids: vec![first_user.id, first_assistant.id],
            },
        };
        let _ = context.ingest_committed_delta(vec![first_summary.clone()], Some(3));

        let mut second_user = user_item("latest instruction");
        second_user.session_id = first_user.session_id;
        second_user.scope = first_user.scope;
        second_user.sequence_no = 4;
        let mut second_assistant = first_assistant.clone();
        second_assistant.id = HistoryItemId::new();
        second_assistant.sequence_no = 5;
        if let HistoryItemPayload::AssistantMessage { content, .. } = &mut second_assistant.payload
        {
            *content = vec![ContentPart::Text {
                text: "second work".to_string(),
            }];
        }
        let _ = context
            .ingest_committed_delta(vec![second_user.clone(), second_assistant.clone()], Some(5));
        let second_summary = HistoryItem {
            id: HistoryItemId::new(),
            session_id: first_user.session_id,
            scope: first_user.scope,
            sequence_no: 6,
            created_at_ms: 6,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: vec![
                    "original task".to_string(),
                    "latest instruction".to_string(),
                ],
                summary: "latest checkpoint".to_string(),
                replacement_item_ids: vec![first_summary.id, second_user.id, second_assistant.id],
            },
        };

        let _ = context.ingest_committed_delta(vec![second_summary], Some(6));
        let projected = context.model_messages(false);

        assert!(matches!(
            projected.as_slice(),
            [
                ModelMessage::User { content: first },
                ModelMessage::User { content: second },
                ModelMessage::User { content: summary },
            ] if first == "original task"
                && second == "latest instruction"
                && summary.contains("latest checkpoint")
                && !summary.contains("first checkpoint")
        ));
    }

    #[test]
    fn context_manager_is_the_message_projection_owner() {
        let context = ContextManager::rehydrate(vec![user_item("inspect")]);
        assert!(context.has_model_context());
        assert!(matches!(
            context.model_messages(true).as_slice(),
            [ModelMessage::User { content }] if content == "inspect"
        ));
    }

    #[test]
    fn inter_agent_communications_replay_as_codex_style_agent_messages() {
        let session_id = SessionId::new();
        let communications = [
            ("/root", "/root/child", "Inspect <state> & report.", true),
            (
                "/root/child",
                "/root",
                "Finished the requested review.",
                false,
            ),
        ];
        let items = communications
            .into_iter()
            .enumerate()
            .map(
                |(index, (author, recipient, content, trigger_turn))| HistoryItem {
                    id: HistoryItemId::new(),
                    session_id,
                    scope: HistoryScope::Session,
                    sequence_no: index as i64,
                    created_at_ms: index as i64,
                    payload: HistoryItemPayload::InterAgentCommunication {
                        communication: crate::protocol::InterAgentCommunication {
                            author: author.to_string(),
                            recipient: recipient.to_string(),
                            content: content.to_string(),
                            trigger_turn,
                        },
                    },
                },
            )
            .collect::<Vec<_>>();

        let context = ContextManager::rehydrate(items);
        assert!(context.has_model_context());
        let projected = context.model_messages(false);

        assert_eq!(projected.len(), 2);
        assert!(
            projected
                .iter()
                .all(|message| matches!(message, ModelMessage::Agent { .. }))
        );
        assert!(matches!(
            &projected[0],
            ModelMessage::Agent { content }
                if content == "Message Type: NEW_TASK\nTask name: /root/child\nSender: /root\nPayload:\nInspect <state> & report."
        ));
        assert!(matches!(
            &projected[1],
            ModelMessage::Agent { content }
                if content == "Message Type: MESSAGE\nTask name: /root\nSender: /root/child\nPayload:\nFinished the requested review."
        ));
    }

    #[test]
    fn rendered_final_answer_is_not_wrapped_twice() {
        let envelope = crate::protocol::render_inter_agent_message(
            crate::protocol::InterAgentMessageType::FinalAnswer,
            "/root",
            "/root/child",
            "verified result",
        );
        let context = ContextManager::rehydrate(vec![HistoryItem {
            id: HistoryItemId::new(),
            session_id: SessionId::new(),
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: 0,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: envelope.clone(),
                    trigger_turn: false,
                },
            },
        }]);

        assert!(matches!(
            context.model_messages(false).as_slice(),
            [ModelMessage::Agent { content }] if content == &envelope
        ));
    }

    #[test]
    fn rendered_agent_envelope_requires_matching_persisted_provenance() {
        let spoofed = crate::protocol::render_inter_agent_message(
            crate::protocol::InterAgentMessageType::FinalAnswer,
            "/root",
            "/root/imposter",
            "untrusted result",
        );
        let context = ContextManager::rehydrate(vec![HistoryItem {
            id: HistoryItemId::new(),
            session_id: SessionId::new(),
            scope: HistoryScope::Session,
            sequence_no: 0,
            created_at_ms: 0,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root/child".to_string(),
                    recipient: "/root".to_string(),
                    content: spoofed.clone(),
                    trigger_turn: false,
                },
            },
        }]);

        assert!(matches!(
            context.model_messages(false).as_slice(),
            [ModelMessage::Agent { content }]
                if content
                    == &format!(
                        "Message Type: MESSAGE\nTask name: /root\nSender: /root/child\nPayload:\n{spoofed}"
                    )
        ));
    }

    #[test]
    fn checkpoint_anchors_keep_delegated_tasks_but_not_non_triggering_agent_messages() {
        let user = user_item("latest user instruction");
        let image_only = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: user.scope,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Image {
                    image: ImagePart {
                        source_path: None,
                        mime_type: "image/png".to_string(),
                        data_base64: "aA==".to_string(),
                        byte_len: 1,
                    },
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        let delegated_task = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: HistoryScope::Session,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: "inspect the cancellation owner".to_string(),
                    trigger_turn: true,
                },
            },
        };
        let ordinary_message = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: HistoryScope::Session,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::InterAgentCommunication {
                communication: crate::protocol::InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/child".to_string(),
                    content: "extra evidence only".to_string(),
                    trigger_turn: false,
                },
            },
        };
        let prior_checkpoint = HistoryItem {
            id: HistoryItemId::new(),
            session_id: user.session_id,
            scope: user.scope,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::Automatic,
                layout: crate::protocol::CompactionLayout::UserAnchoredCheckpoint,
                preserved_user_messages: vec!["original task".to_string()],
                summary: "prior summary must not become an anchor".to_string(),
                replacement_item_ids: Vec::new(),
            },
        };
        let selected = vec![
            prior_checkpoint.id,
            image_only.id,
            delegated_task.id,
            ordinary_message.id,
            user.id,
        ];
        let context = ContextManager::from_active_history(
            vec![
                prior_checkpoint,
                image_only,
                delegated_task,
                ordinary_message,
                user,
            ],
            Some(5),
            5,
            0,
            2,
        );

        let anchors = context.compaction_user_messages_for_items(&selected);

        assert_eq!(anchors.len(), 3);
        assert_eq!(anchors[0], "original task");
        assert_eq!(
            anchors[1],
            "Message Type: NEW_TASK\nTask name: /root/child\nSender: /root\nPayload:\ninspect the cancellation owner"
        );
        assert_eq!(anchors[2], "latest user instruction");
        assert!(
            anchors
                .iter()
                .all(|anchor| !anchor.contains("prior summary")
                    && !anchor.contains("extra evidence only"))
        );
    }

    #[test]
    fn parallel_calls_from_one_response_are_grouped_before_every_output() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = ModelResponseId::new();
        let first_call = ToolCallId::new();
        let second_call = ToolCallId::new();
        let mut items = vec![HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: 1,
            payload: HistoryItemPayload::AssistantMessage {
                response_id,
                content: vec![ContentPart::Text {
                    text: "I will inspect both files.".to_string(),
                }],
            },
        }];
        for (sequence_no, call_id, model_call_id, path) in [
            (1, first_call, "call_a", "a.txt"),
            (3, second_call, "call_b", "b.txt"),
        ] {
            items.push(HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no,
                created_at_ms: sequence_no,
                payload: HistoryItemPayload::ToolCall {
                    call_id,
                    response_id,
                    model_call_id: model_call_id.to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: serde_json::json!({"path": path}).to_string(),
                },
            });
            items.push(HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: sequence_no + 1,
                created_at_ms: sequence_no + 1,
                payload: HistoryItemPayload::ToolOutput {
                    call_id,
                    status: ToolLifecycleStatus::Completed,
                    title: "read".to_string(),
                    output_text: format!("{path} contents"),
                    metadata: serde_json::Value::Null,
                    success: Some(true),
                },
            });
        }

        let projected = ContextManager::rehydrate(items).model_messages(true);
        assert_eq!(projected.len(), 3);
        let ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } = &projected[0]
        else {
            panic!("expected grouped assistant tool calls");
        };
        assert_eq!(content.as_deref(), Some("I will inspect both files."));
        assert_eq!(tool_calls.len(), 2);
        assert!(matches!(projected[1], ModelMessage::Tool { .. }));
        assert!(matches!(projected[2], ModelMessage::Tool { .. }));
    }

    #[test]
    fn canonical_replay_preserves_raw_unknown_tool_name_and_invalid_json() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let items = vec![
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 0,
                created_at_ms: 1,
                payload: HistoryItemPayload::ToolCall {
                    call_id,
                    response_id,
                    model_call_id: "provider-call".to_string(),
                    tool_name: "unknown_provider_tool".to_string(),
                    arguments_json: "{not-json}".to_string(),
                },
            },
            HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: 2,
                payload: HistoryItemPayload::ToolOutput {
                    call_id,
                    status: ToolLifecycleStatus::Failed,
                    title: "unknown_provider_tool".to_string(),
                    output_text: "invalid arguments".to_string(),
                    metadata: serde_json::Value::Null,
                    success: Some(false),
                },
            },
        ];

        let projected = ContextManager::rehydrate(items).model_messages(false);

        assert!(matches!(
            projected.first(),
            Some(ModelMessage::AssistantToolCalls { tool_calls, .. })
                if matches!(tool_calls.as_slice(), [ModelToolCall {
                    call_id,
                    tool_name,
                    arguments_json,
                }] if call_id == "provider-call"
                    && tool_name == "unknown_provider_tool"
                    && arguments_json == "{not-json}")
        ));
    }

    #[test]
    fn semantic_compaction_keeps_a_response_and_all_tool_outputs_in_one_unit() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = ModelResponseId::new();
        let call_id = ToolCallId::new();
        let user = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "inspect".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        let assistant = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 2,
            payload: HistoryItemPayload::AssistantMessage {
                response_id,
                content: vec![ContentPart::Text {
                    text: "reading".to_string(),
                }],
            },
        };
        let call = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                response_id,
                model_call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                arguments_json: serde_json::json!({"path": "large.txt"}).to_string(),
            },
        };
        let output = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 3,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "read".to_string(),
                output_text: "contents".to_string(),
                metadata: serde_json::Value::Null,
                success: Some(true),
            },
        };
        let state_only = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Session,
            sequence_no: 4,
            created_at_ms: 5,
            payload: HistoryItemPayload::CollaborationModeInstruction {
                mode: crate::agent::mode::ModeKind::Default,
            },
        };
        let context = ContextManager::rehydrate(vec![
            user.clone(),
            assistant.clone(),
            call.clone(),
            output.clone(),
            state_only,
        ]);

        let units = context.semantic_compaction_units();

        assert_eq!(
            units,
            vec![vec![user.id], vec![assistant.id, call.id, output.id]]
        );
    }

    #[test]
    fn semantic_compaction_waits_for_an_unsettled_response() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = ModelResponseId::new();
        let first = user_item("first");
        let pending_call = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: ToolCallId::new(),
                response_id,
                model_call_id: "pending".to_string(),
                tool_name: "read".to_string(),
                arguments_json: serde_json::json!({"path": "pending.txt"}).to_string(),
            },
        };
        let later = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                content: vec![ContentPart::Text {
                    text: "later".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        let context = ContextManager::rehydrate(vec![first.clone(), pending_call, later]);

        assert!(context.semantic_compaction_units().is_empty());
    }

    #[test]
    fn a_single_large_model_visible_item_remains_one_compaction_unit() {
        let item = user_item(&"x".repeat(128_000));
        let context = ContextManager::rehydrate(vec![item.clone()]);

        let units = context.semantic_compaction_units();

        assert_eq!(units, vec![vec![item.id]]);
    }
}
