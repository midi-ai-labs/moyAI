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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryChange {
    Unchanged,
    Appended,
    Compacted,
    Rewritten,
}

impl ContextManager {
    pub fn rehydrate(history_items: Vec<HistoryItem>) -> Self {
        let revision = revision_for(&history_items);
        Self {
            history_items,
            revision,
        }
    }

    pub fn replace_committed_history(&mut self, history_items: Vec<HistoryItem>) -> HistoryChange {
        let revision = revision_for(&history_items);
        let change = if revision == self.revision {
            HistoryChange::Unchanged
        } else if history_items.len() >= self.history_items.len()
            && self
                .history_items
                .iter()
                .zip(&history_items)
                .all(|(before, after)| canonical_item_eq(before, after))
        {
            if history_items[self.history_items.len()..]
                .iter()
                .any(|item| matches!(item.payload, HistoryItemPayload::Compaction { .. }))
            {
                HistoryChange::Compacted
            } else {
                HistoryChange::Appended
            }
        } else {
            HistoryChange::Rewritten
        };
        self.history_items = history_items;
        self.revision = revision;
        change
    }

    pub fn revision(&self) -> &HistoryRevision {
        &self.revision
    }

    pub fn history_items(&self) -> &[HistoryItem] {
        &self.history_items
    }

    pub fn has_user_turn(&self) -> bool {
        self.history_items
            .iter()
            .any(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
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

fn canonical_item_eq(before: &HistoryItem, after: &HistoryItem) -> bool {
    before.id == after.id
        && before.session_id == after.session_id
        && before.turn_id == after.turn_id
        && before.sequence_no == after.sequence_no
        && before.created_at_ms == after.created_at_ms
        && serde_json::to_value(&before.payload).ok() == serde_json::to_value(&after.payload).ok()
}

fn revision_for(items: &[HistoryItem]) -> HistoryRevision {
    let mut hash = Sha256::new();
    for item in items {
        hash.update(item.id.to_string().as_bytes());
        hash.update(item.turn_id.to_string().as_bytes());
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
            HistoryItemPayload::InterAgentCommunication { communication } => projected.push((
                index,
                1,
                ModelMessage::Assistant {
                    content: serde_json::to_string(communication).unwrap_or_else(|_| {
                        format!(
                            "Message from {} to {}: {}",
                            communication.author, communication.recipient, communication.content
                        )
                    }),
                },
            )),
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
                projected.push((
                    insertion_index,
                    0,
                    ModelMessage::System {
                        content: format!(
                            "Earlier conversation context was compacted.\n{}",
                            summary.trim()
                        ),
                    },
                ));
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
    use crate::protocol::{ModelResponseId, ToolLifecycleStatus, TurnId};
    use crate::session::{SessionId, ToolCallId};

    fn user_item(text: &str) -> HistoryItem {
        let session_id = SessionId::new();
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: TurnId::new(),
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
        let mut context = ContextManager::rehydrate(vec![first.clone()]);
        let revision = context.revision().clone();
        assert_eq!(
            context.replace_committed_history(vec![first]),
            HistoryChange::Unchanged
        );
        assert_eq!(context.revision(), &revision);
        assert_eq!(
            context.replace_committed_history(vec![user_item("two")]),
            HistoryChange::Rewritten
        );
    }

    #[test]
    fn context_manager_is_the_message_projection_owner() {
        let context = ContextManager::rehydrate(vec![user_item("inspect")]);
        assert!(context.has_user_turn());
        assert!(matches!(
            context.model_messages(true).as_slice(),
            [ModelMessage::User { content }] if content == "inspect"
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
            turn_id,
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
                turn_id,
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
                turn_id,
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
                turn_id,
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
                turn_id,
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
            turn_id,
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
            turn_id,
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
            turn_id,
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
            turn_id,
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
            turn_id,
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
            turn_id,
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
            turn_id,
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
