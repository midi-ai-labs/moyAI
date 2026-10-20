use std::borrow::Cow;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::edit::{PatchOperation, PatchParser};
#[cfg(test)]
use crate::protocol::{ContentPart, HistoryItemPayload};
use crate::protocol::{HistoryItem, HistoryItemId, ToolLifecycleStatus};
use crate::session::{SessionId, ToolCallId};

pub(super) const SOURCE_RECORDS_BEGIN: &str = "<<<SOURCE_RECORDS_BEGIN>>>";
pub(super) const SOURCE_RECORDS_END: &str = "<<<SOURCE_RECORDS_END>>>";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecords {
    version: u32,
    records: Vec<SourceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    source_session_id: SessionId,
    history_item_id: HistoryItemId,
    data: SourceRecordData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceRecordData {
    UserTurn {
        content: Vec<SourceContent>,
    },
    SteerTurn {
        content: Vec<SourceContent>,
    },
    TriggeringAgentInput {
        author: String,
        recipient: String,
        text: String,
    },
    ToolCall {
        call_id: ToolCallId,
        model_call_id: String,
        tool_name: String,
        arguments_json: String,
    },
    ToolCallWithPayloadReferences {
        call_id: ToolCallId,
        model_call_id: String,
        tool_name: String,
        arguments_json_sha256: String,
        arguments_json_utf8_bytes: usize,
        retained_arguments: serde_json::Map<String, serde_json::Value>,
        payload_references: Vec<SourcePayloadReference>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        patch_operations: Vec<SourcePatchOperation>,
    },
    ToolOutput {
        call_id: ToolCallId,
        status: ToolLifecycleStatus,
        success: Option<bool>,
        output_text_sha256: String,
        output_text_utf8_bytes: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePayloadReference {
    field: String,
    utf8_bytes: usize,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum SourcePatchOperation {
    Add {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceContent {
    Text {
        text: String,
    },
    ImageReference {
        source_path: Option<String>,
        mime_type: String,
        declared_image_bytes: u64,
        data_base64_utf8_bytes: usize,
        data_base64_utf8_sha256: String,
    },
}

/// The semantic body has no mandatory layout or size. Framing belongs only to
/// the host, which appends canonical protocol data after accepting the model text.
pub(super) fn validate_checkpoint_body(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("checkpoint body is empty".into());
    }
    if body.contains(SOURCE_RECORDS_BEGIN) || body.contains(SOURCE_RECORDS_END) {
        return Err("checkpoint body contains reserved source-record framing".into());
    }
    Ok(())
}

/// A checkpoint replaces the selected history; it does not carry another copy
/// of every call. The immutable originals remain available through read_history.
pub(super) fn checkpoint_with_history_reference(
    body: &str,
    items: &[HistoryItem],
) -> Result<String, String> {
    validate_checkpoint_body(body)?;
    let Some(first) = items.first() else {
        return Ok(body.to_string());
    };
    let last = items.last().expect("nonempty selected history");
    Ok(format!(
        "{body}\n\nHistorical evidence in this conversation: first_item_id={}; last_item_id={}. Use read_history to retrieve original records. These references do not establish success, approval, or verification.",
        first.id, last.id,
    ))
}

/// Copies protocol evidence from the selected canonical rows. Source IDs retain
/// their original session namespace through forks; they confer no execution or
/// user authority. This does not derive state or recover omitted semantic facts.
#[cfg(test)]
pub(super) fn checkpoint_with_source_records(
    body: &str,
    items: &[HistoryItem],
) -> Result<String, String> {
    validate_checkpoint_body(body)?;
    let mut records = Vec::new();
    let mut seen = HashMap::new();
    for item in items {
        if let HistoryItemPayload::Compaction { summary, .. } = &item.payload {
            if let Some(previous) = inherited_source_records(summary) {
                for record in previous.records {
                    append_record(&mut records, &mut seen, record)?;
                }
            }
            continue;
        }
        let data = match &item.payload {
            HistoryItemPayload::UserTurn { content, .. } => Some(SourceRecordData::UserTurn {
                content: source_content(content),
            }),
            HistoryItemPayload::SteerTurn { content, .. } => Some(SourceRecordData::SteerTurn {
                content: source_content(content),
            }),
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.trigger_turn =>
            {
                Some(SourceRecordData::TriggeringAgentInput {
                    author: communication.author.clone(),
                    recipient: communication.recipient.clone(),
                    text: communication.content.clone(),
                })
            }
            HistoryItemPayload::ToolCall {
                call_id,
                model_call_id,
                tool_name,
                arguments_json,
                ..
            } => Some(SourceRecordData::ToolCall {
                call_id: *call_id,
                model_call_id: model_call_id.clone(),
                tool_name: tool_name.clone(),
                arguments_json: arguments_json.clone(),
            }),
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                output_text,
                ..
            } => Some(SourceRecordData::ToolOutput {
                call_id: *call_id,
                status: *status,
                success: *success,
                output_text_sha256: sha256(output_text),
                output_text_utf8_bytes: output_text.len(),
            }),
            _ => None,
        };
        if let Some(data) = data {
            append_record(
                &mut records,
                &mut seen,
                SourceRecord {
                    source_session_id: item.session_id,
                    history_item_id: item.id,
                    data,
                },
            )?;
        }
    }
    let card = serde_json::to_string(&SourceRecords {
        version: 2,
        records,
    })
    .map_err(|error| format!("could not encode checkpoint source records: {error}"))?;
    Ok(format!(
        "{body}\n\n{SOURCE_RECORDS_BEGIN}\n{card}\n{SOURCE_RECORDS_END}"
    ))
}

fn append_record(
    records: &mut Vec<SourceRecord>,
    seen: &mut HashMap<(SessionId, HistoryItemId), usize>,
    mut record: SourceRecord,
) -> Result<(), String> {
    // Legacy attachments and newly selected canonical calls must receive the
    // same projection before comparing their source identities.
    record.data = reference_tool_call_payloads(record.data);
    let key = (record.source_session_id, record.history_item_id);
    if let Some(index) = seen.get(&key) {
        if records[*index] != record {
            return Err(format!(
                "conflicting checkpoint records for source session {} item {}",
                record.source_session_id, record.history_item_id
            ));
        }
    } else {
        seen.insert(key, records.len());
        records.push(record);
    }
    Ok(())
}

fn reference_tool_call_payloads(data: SourceRecordData) -> SourceRecordData {
    let SourceRecordData::ToolCall {
        call_id,
        model_call_id,
        tool_name,
        arguments_json,
    } = &data
    else {
        return data;
    };
    if let Some((retained_arguments, payload_references, patch_operations)) =
        projected_payload_arguments(tool_name, arguments_json)
    {
        let candidate = SourceRecordData::ToolCallWithPayloadReferences {
            call_id: *call_id,
            model_call_id: model_call_id.clone(),
            tool_name: tool_name.clone(),
            arguments_json_sha256: sha256(arguments_json),
            arguments_json_utf8_bytes: arguments_json.len(),
            retained_arguments,
            payload_references,
            patch_operations,
        };
        // A short literal can carry more useful information in fewer bytes
        // than its references. Compare the actual attachment representation.
        if let (Ok(candidate_bytes), Ok(raw_bytes)) =
            (serde_json::to_vec(&candidate), serde_json::to_vec(&data))
            && candidate_bytes.len() < raw_bytes.len()
        {
            return candidate;
        }
    }
    data
}

type ProjectedPayloadArguments = (
    serde_json::Map<String, serde_json::Value>,
    Vec<SourcePayloadReference>,
    Vec<SourcePatchOperation>,
);

fn projected_payload_arguments(
    tool_name: &str,
    arguments_json: &str,
) -> Option<ProjectedPayloadArguments> {
    // Only these built-in contracts identify file text. Long commands, paths,
    // user inputs, and unknown tool arguments remain exact regardless of size.
    let field = match tool_name {
        "write" => "content",
        "apply_patch" => "patch_text",
        _ => return None,
    };
    let serde_json::Value::Object(mut arguments) = serde_json::from_str(arguments_json).ok()?
    else {
        return None;
    };
    let payload = arguments.get(field)?.as_str()?;
    let patch_operations = if tool_name == "write" {
        arguments.get("path")?.as_str()?;
        Vec::new()
    } else {
        PatchParser::parse(payload)
            .ok()?
            .into_iter()
            .map(|operation| match operation {
                PatchOperation::Add { path, .. } => SourcePatchOperation::Add {
                    path: path.to_string(),
                },
                PatchOperation::Update { path, move_to, .. } => SourcePatchOperation::Update {
                    path: path.to_string(),
                    move_to: move_to.map(|path| path.to_string()),
                },
                PatchOperation::Delete { path } => SourcePatchOperation::Delete {
                    path: path.to_string(),
                },
            })
            .collect()
    };
    let payload_references = vec![SourcePayloadReference {
        field: field.into(),
        utf8_bytes: payload.len(),
        sha256: sha256(payload),
    }];
    arguments.remove(field);
    Some((arguments, payload_references, patch_operations))
}

/// Projects a supported legacy attachment for model input without rewriting its
/// canonical summary or semantic body. Unrecognized data is left untouched.
pub(super) fn project_checkpoint_summary(summary: &str) -> Cow<'_, str> {
    let Some((body, previous)) = split_source_records(summary) else {
        return Cow::Borrowed(summary);
    };
    let mut records = Vec::new();
    let mut seen = HashMap::new();
    for record in previous.records.iter().cloned() {
        if append_record(&mut records, &mut seen, record).is_err() {
            return Cow::Borrowed(summary);
        }
    }
    // Legacy cards remain durable, but their ever-growing Tool inventory is
    // not part of the new model input. Keep attributed input text unchanged.
    records.retain(|record| {
        matches!(
            record.data,
            SourceRecordData::UserTurn { .. }
                | SourceRecordData::SteerTurn { .. }
                | SourceRecordData::TriggeringAgentInput { .. }
        )
    });
    if records.is_empty() {
        return Cow::Owned(format!(
            "{body}\n\nUse read_history to inspect available historical records. Originals not copied into this conversation may be unavailable."
        ));
    }
    if previous.version == 2 && previous.records == records {
        return Cow::Borrowed(summary);
    }
    let Ok(card) = serde_json::to_string(&SourceRecords {
        version: 2,
        records,
    }) else {
        return Cow::Borrowed(summary);
    };
    Cow::Owned(format!(
        "{body}\n{SOURCE_RECORDS_BEGIN}\n{card}\n{SOURCE_RECORDS_END}"
    ))
}

#[cfg(test)]
fn source_content(parts: &[ContentPart]) -> Vec<SourceContent> {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => SourceContent::Text { text: text.clone() },
            ContentPart::Image { image } => SourceContent::ImageReference {
                source_path: image.source_path.as_ref().map(ToString::to_string),
                mime_type: image.mime_type.clone(),
                declared_image_bytes: image.byte_len,
                data_base64_utf8_bytes: image.data_base64.len(),
                data_base64_utf8_sha256: sha256(&image.data_base64),
            },
        })
        .collect()
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
fn inherited_source_records(summary: &str) -> Option<SourceRecords> {
    split_source_records(summary).map(|(_, card)| card)
}

fn split_source_records(summary: &str) -> Option<(&str, SourceRecords)> {
    let (body, suffix) = summary.rsplit_once(&format!("\n{SOURCE_RECORDS_BEGIN}\n"))?;
    let json = suffix.strip_suffix(&format!("\n{SOURCE_RECORDS_END}"))?;
    let card: SourceRecords = serde_json::from_str(json).ok()?;
    matches!(card.version, 1 | 2).then_some((body, card))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CompactionLayout, CompactionMode, HistoryScope, InterAgentCommunication, ModelResponseId,
        TurnId,
    };
    use crate::session::ImagePart;

    fn item(payload: HistoryItemPayload) -> HistoryItem {
        HistoryItem {
            id: HistoryItemId::new(),
            session_id: SessionId::new(),
            scope: HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: 0,
            created_at_ms: 0,
            payload,
        }
    }

    fn user(text: &str) -> HistoryItem {
        item(HistoryItemPayload::UserTurn {
            content: vec![ContentPart::Text { text: text.into() }],
            prompt_dispatch: None,
            editor_context: None,
        })
    }

    fn checkpoint(summary: String) -> HistoryItem {
        item(HistoryItemPayload::Compaction {
            mode: CompactionMode::Automatic,
            layout: CompactionLayout::UserAnchoredCheckpoint,
            preserved_user_messages: Vec::new(),
            clm_checkpoint: None,
            summary,
            replacement_item_ids: Vec::new(),
        })
    }

    fn records(summary: &str) -> Vec<SourceRecord> {
        inherited_source_records(summary).unwrap().records
    }

    #[test]
    fn free_form_body_is_unchanged_and_only_host_framing_is_reserved() {
        let body = "  arbitrary facts; observed ≠ verified\r\n## A heading\nunfinished: unknown\n";
        let summary = checkpoint_with_source_records(body, &[]).unwrap();
        assert!(summary.starts_with(&format!("{body}\n\n{SOURCE_RECORDS_BEGIN}\n")));
        assert!(records(&summary).is_empty());
        for invalid in ["", " \n\t", SOURCE_RECORDS_BEGIN, SOURCE_RECORDS_END] {
            assert!(validate_checkpoint_body(invalid).is_err());
            assert!(checkpoint_with_source_records(invalid, &[]).is_err());
        }
        assert!(validate_checkpoint_body(&"long useful evidence ".repeat(4_000)).is_ok());
    }

    #[test]
    fn input_parts_and_revisions_are_exact_without_copying_image_payloads() {
        let text = "keep src/相対.txt; \\\"value\\\"\r\nnot tested";
        let mut original = user(text);
        let image_data = "aGlzdG9yaWNhbC1pbWFnZS1wYXlsb2Fk";
        if let HistoryItemPayload::UserTurn { content, .. } = &mut original.payload {
            content.push(ContentPart::Image {
                image: ImagePart {
                    source_path: Some("images/input.png".into()),
                    mime_type: "image/png".into(),
                    data_base64: image_data.into(),
                    byte_len: 24,
                },
            });
        }
        let revision = item(HistoryItemPayload::SteerTurn {
            expected_turn_id: TurnId::new(),
            content: vec![ContentPart::Text { text: text.into() }],
            additional_context: Default::default(),
            client_user_message_id: None,
        });
        let summary = checkpoint_with_source_records(
            "semantic body",
            &[original.clone(), revision.clone(), original.clone()],
        )
        .unwrap();
        let copied = records(&summary);
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].history_item_id, original.id);
        assert_eq!(copied[1].history_item_id, revision.id);
        let SourceRecordData::UserTurn { content } = &copied[0].data else {
            panic!("user record")
        };
        assert_eq!(content[0], SourceContent::Text { text: text.into() });
        let SourceContent::ImageReference {
            data_base64_utf8_sha256,
            data_base64_utf8_bytes,
            declared_image_bytes,
            ..
        } = &content[1]
        else {
            panic!("image reference")
        };
        assert_eq!(data_base64_utf8_sha256, &sha256(image_data));
        assert_eq!(*data_base64_utf8_bytes, image_data.len());
        assert_eq!(*declared_image_bytes, 24);
        assert_eq!(
            copied[1].data,
            SourceRecordData::SteerTurn {
                content: vec![SourceContent::Text { text: text.into() }],
            }
        );
        assert!(!summary.contains(image_data));
    }

    #[test]
    fn calls_keep_raw_arguments_and_outputs_keep_only_typed_references() {
        let call_id = ToolCallId::new();
        let arguments = format!(
            "{{ \"path\":\"src/x.txt\",\"value\":\"{}\" }}",
            "large".repeat(3_000)
        );
        let call = item(HistoryItemPayload::ToolCall {
            call_id,
            response_id: ModelResponseId::new(),
            model_call_id: "provider-call-1".into(),
            tool_name: "external_tool_name".into(),
            arguments_json: arguments.clone(),
        });
        let invalid_arguments = "{ invalid JSON; keep the original text";
        let invalid = item(HistoryItemPayload::ToolCall {
            call_id: ToolCallId::new(),
            response_id: ModelResponseId::new(),
            model_call_id: String::new(),
            tool_name: "read".into(),
            arguments_json: invalid_arguments.into(),
        });
        let output = "not a completed verification\r\n観測値";
        let result = item(HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Failed,
            title: "not copied".into(),
            output_text: output.into(),
            metadata: serde_json::json!({"private": "metadata_must_not_be_copied"}),
            success: Some(false),
        });
        let assistant = item(HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: "assistant_body_is_not_a_protocol_record".into(),
            }],
        });
        let summary = checkpoint_with_source_records(
            "continue inspection",
            &[call.clone(), result.clone(), invalid, assistant],
        )
        .unwrap();
        let copied = records(&summary);
        assert_eq!(copied.len(), 3);
        assert!(matches!(&copied[0].data, SourceRecordData::ToolCall {
            call_id: copied_id, model_call_id, tool_name, arguments_json
        } if *copied_id == call_id && model_call_id == "provider-call-1"
            && tool_name == "external_tool_name" && arguments_json == &arguments));
        assert_eq!(copied[1].source_session_id, result.session_id);
        assert_eq!(copied[1].history_item_id, result.id);
        assert_eq!(
            copied[1].data,
            SourceRecordData::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Failed,
                success: Some(false),
                output_text_sha256: sha256(output),
                output_text_utf8_bytes: output.len(),
            }
        );
        assert!(
            matches!(&copied[2].data, SourceRecordData::ToolCall { arguments_json, .. }
            if arguments_json == invalid_arguments)
        );
        for excluded in [
            output,
            "metadata_must_not_be_copied",
            "assistant_body_is_not_a_protocol_record",
        ] {
            assert!(!summary.contains(excluded));
        }
    }

    #[test]
    fn only_triggering_agent_inputs_are_kept_and_not_labeled_as_user_turns() {
        let communication = InterAgentCommunication {
            author: "/root".into(),
            recipient: "/root/child".into(),
            content: "inspect the owner; not an actual user quotation".into(),
            trigger_turn: true,
        };
        let triggering = item(HistoryItemPayload::InterAgentCommunication {
            communication: communication.clone(),
        });
        let ordinary = item(HistoryItemPayload::InterAgentCommunication {
            communication: InterAgentCommunication {
                trigger_turn: false,
                ..communication.clone()
            },
        });
        let copied =
            records(&checkpoint_with_source_records("facts", &[triggering, ordinary]).unwrap());
        assert_eq!(copied.len(), 1);
        assert_eq!(
            copied[0].data,
            SourceRecordData::TriggeringAgentInput {
                author: communication.author,
                recipient: communication.recipient,
                text: communication.content,
            }
        );
    }

    #[test]
    fn repeated_compaction_flattens_records_in_their_source_session_namespaces() {
        let original = user("original instruction");
        let first = checkpoint(
            checkpoint_with_source_records("old_semantic_only", &[original.clone()]).unwrap(),
        );
        let revision = user("new instruction");
        let second = checkpoint(
            checkpoint_with_source_records(
                "second_semantic_only",
                &[first.clone(), revision.clone()],
            )
            .unwrap(),
        );
        let mut same_id_other_session = original.clone();
        same_id_other_session.session_id = SessionId::new();
        let combined = checkpoint_with_source_records(
            "latest body",
            &[first, second, revision, same_id_other_session.clone()],
        )
        .unwrap();
        let copied = records(&combined);
        assert_eq!(copied.len(), 3);
        assert_eq!(copied[0].history_item_id, original.id);
        assert_eq!(copied[0].source_session_id, original.session_id);
        assert_eq!(copied[2].history_item_id, original.id);
        assert_eq!(
            copied[2].source_session_id,
            same_id_other_session.session_id
        );
        assert_eq!(combined.matches(SOURCE_RECORDS_BEGIN).count(), 1);
        assert_eq!(combined.matches(SOURCE_RECORDS_END).count(), 1);
        assert!(!combined.contains("old_semantic_only"));
        assert!(!combined.contains("second_semantic_only"));
    }

    #[test]
    fn legacy_summaries_do_not_become_canonical_source_records() {
        for old in [
            "## Objective and exact contract\nInherited narrative, not a user quotation.".into(),
            format!("legacy text mentions {SOURCE_RECORDS_BEGIN} inside a sentence"),
            format!(
                "old body\n{SOURCE_RECORDS_BEGIN}\n{{\"version\":99,\"records\":[]}}\n{SOURCE_RECORDS_END}"
            ),
            format!(
                "old body\n{SOURCE_RECORDS_BEGIN}\n{{\"version\":1,\"records\":[{{\"invented\":\"shape\"}}]}}\n{SOURCE_RECORDS_END}"
            ),
        ] {
            let summary =
                checkpoint_with_source_records("latest facts", &[checkpoint(old)]).unwrap();
            assert!(records(&summary).is_empty());
        }
    }

    #[test]
    fn conflicting_values_for_one_source_id_are_not_silently_selected() {
        let original = user("original instruction");
        let prior =
            checkpoint(checkpoint_with_source_records("prior body", &[original.clone()]).unwrap());
        let mut different = original;
        if let HistoryItemPayload::UserTurn { content, .. } = &mut different.payload {
            *content = vec![ContentPart::Text {
                text: "different instruction".into(),
            }];
        }
        assert!(checkpoint_with_source_records("latest body", &[prior, different]).is_err());
    }

    fn tool_call(tool_name: &str, arguments_json: String) -> HistoryItem {
        item(HistoryItemPayload::ToolCall {
            call_id: ToolCallId::new(),
            response_id: ModelResponseId::new(),
            model_call_id: "provider-call-payload".into(),
            tool_name: tool_name.into(),
            arguments_json,
        })
    }

    fn raw_call_record(call: &HistoryItem) -> SourceRecord {
        let HistoryItemPayload::ToolCall {
            call_id,
            model_call_id,
            tool_name,
            arguments_json,
            ..
        } = &call.payload
        else {
            panic!("tool call fixture")
        };
        SourceRecord {
            source_session_id: call.session_id,
            history_item_id: call.id,
            data: SourceRecordData::ToolCall {
                call_id: *call_id,
                model_call_id: model_call_id.clone(),
                tool_name: tool_name.clone(),
                arguments_json: arguments_json.clone(),
            },
        }
    }

    fn legacy_summary(body: &str, records: Vec<SourceRecord>) -> String {
        let card = serde_json::to_string(&SourceRecords {
            version: 1,
            records,
        })
        .unwrap();
        format!("{body}\n\n{SOURCE_RECORDS_BEGIN}\n{card}\n{SOURCE_RECORDS_END}")
    }

    #[test]
    fn write_payload_is_referenced_without_changing_canonical_call_or_failed_result() {
        let payload = "正文\r\nold document content ".repeat(3_000);
        let path = "../Exact\\Relative/文書.MD";
        let arguments = serde_json::json!({
            "path": path,
            "content": payload,
            "extra": {"command": "exact; unchanged"},
        })
        .to_string();
        let call = tool_call("write", arguments.clone());
        let call_before = serde_json::to_value(&call).unwrap();
        let HistoryItemPayload::ToolCall { call_id, .. } = call.payload else {
            unreachable!()
        };
        let output = item(HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Failed,
            title: "write failed".into(),
            output_text: "not committed".into(),
            metadata: serde_json::Value::Null,
            success: Some(false),
        });
        let summary =
            checkpoint_with_source_records("write attempt failed", &[call.clone(), output])
                .unwrap();
        let copied = records(&summary);
        assert_eq!(copied[0].history_item_id, call.id);
        let SourceRecordData::ToolCallWithPayloadReferences {
            call_id: copied_call_id,
            model_call_id,
            tool_name,
            arguments_json_sha256,
            arguments_json_utf8_bytes,
            retained_arguments,
            payload_references,
            patch_operations,
        } = &copied[0].data
        else {
            panic!("write payload reference")
        };
        assert_eq!(*copied_call_id, call_id);
        assert_eq!(model_call_id, "provider-call-payload");
        assert_eq!(tool_name, "write");
        assert_eq!(arguments_json_sha256, &sha256(&arguments));
        assert_eq!(*arguments_json_utf8_bytes, arguments.len());
        assert_eq!(retained_arguments["path"], path);
        assert_eq!(retained_arguments["extra"]["command"], "exact; unchanged");
        assert!(!retained_arguments.contains_key("content"));
        assert_eq!(
            payload_references,
            &vec![SourcePayloadReference {
                field: "content".into(),
                utf8_bytes: payload.len(),
                sha256: sha256(&payload),
            }]
        );
        assert!(patch_operations.is_empty());
        assert!(matches!(
            copied[1].data,
            SourceRecordData::ToolOutput {
                status: ToolLifecycleStatus::Failed,
                success: Some(false),
                ..
            }
        ));
        assert!(!summary.contains("old document content"));
        assert_eq!(serde_json::to_value(&call).unwrap(), call_before);
    }

    #[test]
    fn short_or_empty_file_payloads_keep_exact_raw_arguments_when_references_would_grow() {
        for (tool_name, arguments) in [
            (
                "write",
                r#"{ "path":"flags.txt", "content":"" }"#.to_string(),
            ),
            (
                "write",
                r#"{ "path":"flags.txt", "content":"enabled=true\n" }"#.to_string(),
            ),
            (
                "apply_patch",
                serde_json::json!({
                    "patch_text": "*** Begin Patch\n*** Add File: flags.txt\n+on\n*** End Patch",
                })
                .to_string(),
            ),
            (
                "apply_patch",
                serde_json::json!({
                    "patch_text": "*** Begin Patch\n*** Delete File: flags.txt\n*** End Patch",
                })
                .to_string(),
            ),
        ] {
            let call = tool_call(tool_name, arguments.clone());
            let raw = raw_call_record(&call);
            let summary = checkpoint_with_source_records("literal fact", &[call.clone()]).unwrap();
            assert_eq!(records(&summary), vec![raw.clone()]);
            assert_eq!(raw_call_record(&call), raw);
            let legacy = legacy_summary("literal fact", vec![raw.clone()]);
            let projected = project_checkpoint_summary(&legacy);
            assert!(!projected.contains(SOURCE_RECORDS_BEGIN));
            assert!(projected.contains("read_history"));
            assert_eq!(records(&legacy), vec![raw]);
            assert!(projected.len() <= legacy.len());
        }
    }

    #[test]
    fn patch_payload_keeps_exact_operation_paths_and_references_original_text() {
        let payload = format!(
            "*** Begin Patch\r\n*** Add File: 文書/New.MD\r\n+{}\r\n*** Update File: old\\Name.rs\r\n*** Move to: new\\Name.rs\r\n@@\r\n-old\r\n+new\r\n*** Delete File: obsolete/OLD.txt\r\n*** End Patch",
            "source_payload_must_not_survive ".repeat(2_000),
        );
        let arguments = serde_json::json!({"patch_text": payload, "other": 7}).to_string();
        let call = tool_call("apply_patch", arguments.clone());
        let copied =
            records(&checkpoint_with_source_records("patch was proposed", &[call]).unwrap());
        let SourceRecordData::ToolCallWithPayloadReferences {
            arguments_json_sha256,
            arguments_json_utf8_bytes,
            retained_arguments,
            payload_references,
            patch_operations,
            ..
        } = &copied[0].data
        else {
            panic!("patch reference")
        };
        assert_eq!(arguments_json_sha256, &sha256(&arguments));
        assert_eq!(*arguments_json_utf8_bytes, arguments.len());
        assert_eq!(
            retained_arguments,
            serde_json::json!({"other": 7}).as_object().unwrap()
        );
        assert_eq!(
            payload_references,
            &vec![SourcePayloadReference {
                field: "patch_text".into(),
                utf8_bytes: payload.len(),
                sha256: sha256(&payload),
            }]
        );
        assert_eq!(
            patch_operations,
            &vec![
                SourcePatchOperation::Add {
                    path: "文書/New.MD".into()
                },
                SourcePatchOperation::Update {
                    path: "old\\Name.rs".into(),
                    move_to: Some("new\\Name.rs".into())
                },
                SourcePatchOperation::Delete {
                    path: "obsolete/OLD.txt".into()
                },
            ]
        );
    }

    #[test]
    fn unknown_tools_commands_and_invalid_payload_contracts_keep_raw_arguments() {
        let long = "long exact path / 値 ".repeat(3_000);
        for (name, arguments) in [
            (
                "external_tool_name",
                serde_json::json!({"content": long}).to_string(),
            ),
            (
                "mcp_call",
                serde_json::json!({"arguments": {"content": long}}).to_string(),
            ),
            (
                "shell",
                serde_json::json!({"command": long, "workdir": long}).to_string(),
            ),
            ("read", serde_json::json!({"path": long}).to_string()),
            (
                "compact_context",
                serde_json::json!({"summary": long}).to_string(),
            ),
            ("write", "{ invalid JSON".into()),
            (
                "write",
                serde_json::json!({"path": 1, "content": long}).to_string(),
            ),
            (
                "write",
                serde_json::json!({"path": "a", "content": [long]}).to_string(),
            ),
            ("write", serde_json::json!({"content": long}).to_string()),
            (
                "apply_patch",
                serde_json::json!({"patch_text": "*** Add File: a\n+invalid without framing"})
                    .to_string(),
            ),
            (
                "apply_patch",
                serde_json::json!({"patch_text": 4}).to_string(),
            ),
        ] {
            let call = tool_call(name, arguments.clone());
            let copied = records(&checkpoint_with_source_records("facts", &[call]).unwrap());
            assert!(matches!(&copied[0].data, SourceRecordData::ToolCall {
                tool_name, arguments_json, ..
            } if tool_name == name && arguments_json == &arguments));
        }
        let exact_user = user(&long);
        let copied = records(&checkpoint_with_source_records("facts", &[exact_user]).unwrap());
        assert_eq!(
            copied[0].data,
            SourceRecordData::UserTurn {
                content: vec![SourceContent::Text { text: long }],
            }
        );
    }

    #[test]
    fn legacy_projection_preserves_semantic_body_and_is_idempotent() {
        let call = tool_call(
            "write",
            serde_json::json!({
                "path": "Exact/old.md", "content": "legacy_document_payload ".repeat(2_000),
            })
            .to_string(),
        );
        let body = "  semantic facts\r\nunknown remains unknown\n ";
        let legacy = legacy_summary(body, vec![raw_call_record(&call)]);
        let original = legacy.clone();
        let projected = project_checkpoint_summary(&legacy);
        assert!(projected.starts_with(body));
        assert!(!projected.contains("legacy_document_payload"));
        assert!(!projected.contains(SOURCE_RECORDS_BEGIN));
        assert!(projected.contains("read_history"));
        assert!(matches!(
            project_checkpoint_summary(&projected),
            Cow::Borrowed(_)
        ));
        assert_eq!(legacy, original);
        assert!(projected.len() < legacy.len());
    }

    #[test]
    fn legacy_raw_and_referenced_calls_deduplicate_before_repeat_and_fork() {
        let call = tool_call(
            "write",
            serde_json::json!({
                "path": "a.md", "content": "historical_document_body".repeat(300),
            })
            .to_string(),
        );
        let legacy = checkpoint(legacy_summary("old body", vec![raw_call_record(&call)]));
        let referenced =
            checkpoint(checkpoint_with_source_records("new body", &[call.clone()]).unwrap());
        let mut forked = call.clone();
        forked.session_id = SessionId::new();
        let combined = checkpoint_with_source_records(
            "latest",
            &[legacy.clone(), referenced, call.clone(), forked.clone()],
        )
        .unwrap();
        let copied = records(&combined);
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].source_session_id, call.session_id);
        assert_eq!(copied[1].source_session_id, forked.session_id);
        assert!(!combined.contains("historical_document_body"));
        let repeated = checkpoint_with_source_records("next", &[checkpoint(combined)]).unwrap();
        assert_eq!(records(&repeated), copied);
        let mut changed = call;
        if let HistoryItemPayload::ToolCall { arguments_json, .. } = &mut changed.payload {
            *arguments_json =
                serde_json::json!({"path": "a.md", "content": "different"}).to_string();
        }
        assert!(checkpoint_with_source_records("conflict", &[legacy, changed]).is_err());
    }

    #[test]
    fn unsupported_or_malformed_legacy_projection_is_unchanged() {
        for summary in [
            "plain legacy narrative".into(),
            format!(
                "body\n{SOURCE_RECORDS_BEGIN}\n{{\"version\":99,\"records\":[]}}\n{SOURCE_RECORDS_END}"
            ),
            format!("body\n{SOURCE_RECORDS_BEGIN}\nnot JSON\n{SOURCE_RECORDS_END}"),
            format!(
                "body\n{SOURCE_RECORDS_BEGIN}\n{{\"version\":1,\"records\":[{{\"invented\":true}}]}}\n{SOURCE_RECORDS_END}"
            ),
        ] {
            assert!(matches!(
                project_checkpoint_summary(&summary),
                Cow::Borrowed(_)
            ));
            assert_eq!(project_checkpoint_summary(&summary), summary);
        }
    }

    #[test]
    fn snapshot_reference_does_not_accumulate_tool_records_or_previous_bodies() {
        let old = tool_call("read", "{\"path\":\"source.txt\"}".into());
        let first = checkpoint_with_history_reference("first facts", &[old]).unwrap();
        let previous = checkpoint(first.clone());
        let next = tool_call(
            "write",
            "{\"path\":\"result.txt\",\"content\":\"new text\"}".into(),
        );
        let second =
            checkpoint_with_history_reference("latest facts", &[previous.clone(), next.clone()])
                .unwrap();
        assert!(second.starts_with("latest facts\n\n"));
        assert!(!second.contains("first facts"));
        assert!(!second.contains("arguments_json"));
        assert!(!second.contains(SOURCE_RECORDS_BEGIN));
        assert!(second.contains(&previous.id.to_string()));
        assert!(second.contains(&next.id.to_string()));
        assert_eq!(
            second.len() - "latest facts".len(),
            first.len() - "first facts".len()
        );
    }

    #[test]
    fn legacy_input_provenance_survives_while_tool_inventory_is_omitted() {
        let original = user("exact historical requirement");
        let call = tool_call("read", "{\"path\":\"source.txt\"}".into());
        let legacy =
            checkpoint_with_source_records("observed facts", &[original.clone(), call]).unwrap();
        let projected = project_checkpoint_summary(&legacy);
        let (_, records) = split_source_records(&projected).unwrap();
        assert_eq!(records.records.len(), 1);
        assert_eq!(records.records[0].history_item_id, original.id);
        assert!(projected.contains("exact historical requirement"));
        assert!(!projected.contains("tool_call"));
        assert_eq!(project_checkpoint_summary(&projected), projected);
    }
}
