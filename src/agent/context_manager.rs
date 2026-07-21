use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::llm::{ModelContentPart, ModelMessage, ModelToolCall};
use crate::protocol::{ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload};

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
    /// summary at their earliest model position; replaced raw content is never
    /// reloaded merely to detect this change.
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
                HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::Compaction { .. }
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
        units
            .into_iter()
            .take_while(|(index, _)| {
                first_unsettled_response_index.is_none_or(|blocked| *index < blocked)
            })
            .map(|(_, item_ids)| item_ids)
            .collect()
    }

    pub fn compaction_segments_for_units(
        &self,
        units: &[Vec<HistoryItemId>],
        supports_images: bool,
    ) -> Vec<String> {
        units
            .iter()
            .filter_map(|unit| {
                let rendered = self
                    .model_messages_for_items(unit, supports_images)
                    .iter()
                    .map(render_model_message_for_compaction)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (!rendered.trim().is_empty()).then_some(rendered)
            })
            .collect()
    }
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

fn render_model_message_for_compaction(message: &ModelMessage) -> String {
    match message {
        ModelMessage::System { content } => format!("[system]\n{content}"),
        ModelMessage::User { content } => format!("[user]\n{content}"),
        ModelMessage::UserParts { parts } => {
            let content = parts
                .iter()
                .map(|part| match part {
                    ModelContentPart::Text { text } => text.clone(),
                    ModelContentPart::Image { mime_type, .. } => {
                        format!(
                            "[image input: {mime_type}; binary data omitted from summary request]"
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("[user]\n{content}")
        }
        ModelMessage::Assistant { content } => format!("[assistant]\n{content}"),
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => {
            let calls = tool_calls
                .iter()
                .map(|call| {
                    format!(
                        "tool_call {} {} {}",
                        call.call_id, call.tool_name, call.arguments_json
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "[assistant]\n{}{}{}",
                content.as_deref().unwrap_or_default(),
                if content.is_some() && !calls.is_empty() {
                    "\n"
                } else {
                    ""
                },
                calls
            )
        }
        ModelMessage::Tool {
            call_id,
            tool_name,
            result,
            ..
        } => format!("[tool {tool_name} {call_id}]\n{result}"),
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
    let mut projected = Vec::<(usize, u8, ModelMessage)>::new();
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
                ..
            } => {
                let call_id_text = call_id.to_string();
                if let Some((model_call_id, tool_name)) =
                    tool_names_by_call.get(&call_id_text).cloned()
                {
                    projected.push((
                        index,
                        1,
                        ModelMessage::Tool {
                            call_id: model_call_id,
                            tool_name,
                            result: output_text.clone(),
                            metadata: serde_json::Value::Null,
                        },
                    ));
                }
            }
            HistoryItemPayload::Compaction {
                summary,
                replacement_item_ids,
                ..
            } => {
                let insertion_index = replacement_item_ids
                    .iter()
                    .filter_map(|id| index_by_id.get(id).copied())
                    .min()
                    .unwrap_or(index);
                projected.push((insertion_index, 0, semantic_compaction_message(summary)));
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

pub(super) fn semantic_compaction_message(summary: &str) -> ModelMessage {
    ModelMessage::User {
        content: format!(
            "Earlier conversation context was compacted.\n{}",
            summary.trim()
        ),
    }
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
    ModelMessage::User {
        content: format!(
            "<inter_agent_message author=\"{}\" recipient=\"{}\">\n{}\n</inter_agent_message>",
            escape_model_envelope_text(&communication.author.to_string()),
            escape_model_envelope_text(&communication.recipient.to_string()),
            escape_model_envelope_text(&communication.content),
        ),
    }
}

fn escape_model_envelope_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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
    use super::*;
    use crate::protocol::{HistoryScope, ModelResponseId, ToolLifecycleStatus, TurnId};
    use crate::session::{SessionId, ToolCallId};

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
    fn compaction_delta_replaces_active_raw_items_without_full_rehydrate() {
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
            [ModelMessage::User { content }, ModelMessage::User { content: tail_text }]
                if content.contains("first and second summarized") && tail_text == "tail"
        ));
    }

    #[test]
    fn partial_compaction_anchors_a_retained_tool_suffix_with_user_context() {
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
                summary: "Continue the retained inspection.".to_string(),
                replacement_item_ids: vec![old.id],
            },
        };

        let delta = context.ingest_committed_delta(vec![compaction], Some(5));
        let projected = context.model_messages(false);

        assert_eq!(delta.change, HistoryChange::Compacted);
        assert!(matches!(
            projected.as_slice(),
            [
                ModelMessage::User { content },
                ModelMessage::AssistantToolCalls { tool_calls, .. },
                ModelMessage::Tool { call_id, result, .. }
            ] if content.contains("Continue the retained inspection.")
                && matches!(tool_calls.as_slice(), [ModelToolCall { call_id, .. }] if call_id == "call-retained")
                && call_id == "call-retained"
                && result == "retained contents"
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
    fn inter_agent_communications_replay_as_provenanced_external_input() {
        let session_id = SessionId::new();
        let communications = [
            ("/root", "/root/child", "Inspect <state> & report."),
            ("/root/child", "/root", "Finished the requested review."),
        ];
        let items = communications
            .into_iter()
            .enumerate()
            .map(|(index, (author, recipient, content))| HistoryItem {
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
                        trigger_turn: true,
                    },
                },
            })
            .collect::<Vec<_>>();

        let projected = ContextManager::rehydrate(items).model_messages(false);

        assert_eq!(projected.len(), 2);
        assert!(
            projected
                .iter()
                .all(|message| matches!(message, ModelMessage::User { .. }))
        );
        assert!(matches!(
            &projected[0],
            ModelMessage::User { content }
                if content.contains("author=\"/root\"")
                    && content.contains("recipient=\"/root/child\"")
                    && content.contains("&lt;state&gt; &amp; report.")
        ));
        assert!(matches!(
            &projected[1],
            ModelMessage::User { content }
                if content.contains("author=\"/root/child\"")
                    && content.contains("recipient=\"/root\"")
                    && content.contains("Finished the requested review.")
        ));
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
    fn semantic_compaction_stops_before_an_unsettled_response() {
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

        assert_eq!(context.semantic_compaction_units(), vec![vec![first.id]]);
    }

    #[test]
    fn a_single_large_model_visible_item_is_a_valid_compaction_unit() {
        let item = user_item(&"x".repeat(128_000));
        let context = ContextManager::rehydrate(vec![item.clone()]);

        let units = context.semantic_compaction_units();
        let segments = context.compaction_segments_for_units(&units, false);

        assert_eq!(units, vec![vec![item.id]]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].contains(&"x".repeat(1_024)));
    }
}
