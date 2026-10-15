use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::ModelMessage;

/// Legacy persisted CLM model input, retained for existing-session replay.
/// New compactions use moyAI's user-anchored layout. This is replay data, not a user-authority source or
/// a display summary. Tool metadata has a separate owner because ModelMessage
/// deliberately excludes it from provider-facing serialization.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "ClmCheckpointWire")]
pub struct ClmCheckpoint {
    version: u32,
    messages: Vec<ModelMessage>,
    tool_metadata: BTreeMap<usize, Value>,
    protected_prefix_len: usize,
}

#[derive(Deserialize)]
struct ClmCheckpointWire {
    version: u32,
    messages: Vec<ModelMessage>,
    #[serde(deserialize_with = "deserialize_tool_metadata")]
    tool_metadata: BTreeMap<usize, Value>,
    protected_prefix_len: usize,
}

fn deserialize_tool_metadata<'de, D>(deserializer: D) -> Result<BTreeMap<usize, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct MetadataVisitor;

    impl<'de> serde::de::Visitor<'de> for MetadataVisitor {
        type Value = BTreeMap<usize, Value>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("tool metadata keyed by canonical message indices")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut metadata = BTreeMap::new();
            // JSON object keys stay strings after an outer tagged enum buffers
            // its content. Parse explicitly instead of relying on JSON's direct
            // numeric-map-key adapter, which that buffered path does not use.
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                let index = key.parse::<usize>().map_err(|_| {
                    <A::Error as serde::de::Error>::custom(
                        "CLM metadata key is not a message index",
                    )
                })?;
                if key != index.to_string() {
                    return Err(serde::de::Error::custom(
                        "CLM metadata key is not a canonical message index",
                    ));
                }
                if metadata.insert(index, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "CLM metadata contains a duplicate message index",
                    ));
                }
            }
            Ok(metadata)
        }
    }

    deserializer.deserialize_map(MetadataVisitor)
}

impl TryFrom<ClmCheckpointWire> for ClmCheckpoint {
    type Error = String;

    fn try_from(value: ClmCheckpointWire) -> Result<Self, Self::Error> {
        let checkpoint = Self {
            version: value.version,
            messages: value.messages,
            tool_metadata: value.tool_metadata,
            protected_prefix_len: value.protected_prefix_len,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }
}

impl fmt::Debug for ClmCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClmCheckpoint")
            .field("version", &self.version)
            .field("message_count", &self.messages.len())
            .field("tool_metadata_count", &self.tool_metadata.len())
            .field("protected_prefix_len", &self.protected_prefix_len)
            .finish()
    }
}

impl ClmCheckpoint {
    pub fn from_messages(
        messages: &[ModelMessage],
        protected_prefix_len: usize,
    ) -> Result<Self, String> {
        let mut messages = messages.to_vec();
        let mut tool_metadata = BTreeMap::new();
        for (index, message) in messages.iter_mut().enumerate() {
            if let ModelMessage::Tool { metadata, .. } = message {
                if !metadata.is_null() {
                    tool_metadata.insert(index, std::mem::take(metadata));
                }
            }
        }
        let checkpoint = Self {
            version: 1,
            messages,
            tool_metadata,
            protected_prefix_len,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn messages(&self) -> Result<Vec<ModelMessage>, String> {
        self.validate()?;
        let mut messages = self.messages.clone();
        for (index, value) in &self.tool_metadata {
            if let ModelMessage::Tool { metadata, .. } = &mut messages[*index] {
                *metadata = value.clone();
            }
        }
        Ok(messages)
    }

    pub fn protected_prefix_len(&self) -> usize {
        self.protected_prefix_len
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported CLM checkpoint version".into());
        }
        if self.messages.is_empty() || self.protected_prefix_len > self.messages.len() {
            return Err("CLM checkpoint has an invalid protected prefix".into());
        }
        for index in self.tool_metadata.keys() {
            if !matches!(self.messages.get(*index), Some(ModelMessage::Tool { .. })) {
                return Err("CLM checkpoint metadata must address a tool message".into());
            }
        }
        let mut pending = BTreeMap::new();
        for message in &self.messages {
            match message {
                ModelMessage::System { .. } | ModelMessage::Developer { .. } => {
                    return Err(
                        "CLM checkpoint cannot contain system or developer instructions".into(),
                    );
                }
                ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                    if !pending.is_empty() || tool_calls.is_empty() {
                        return Err("CLM checkpoint contains an incomplete tool call group".into());
                    }
                    for call in tool_calls {
                        if call.call_id.is_empty()
                            || call.tool_name.is_empty()
                            || pending
                                .insert(call.call_id.as_str(), call.tool_name.as_str())
                                .is_some()
                        {
                            return Err(
                                "CLM checkpoint contains invalid or duplicate tool calls".into()
                            );
                        }
                    }
                }
                ModelMessage::Tool {
                    call_id, tool_name, ..
                } => {
                    if pending.remove(call_id.as_str()) != Some(tool_name.as_str()) {
                        return Err("CLM checkpoint tool output does not match its call".into());
                    }
                }
                _ if !pending.is_empty() => {
                    return Err("CLM checkpoint interrupts a native tool call group".into());
                }
                _ => {}
            }
        }
        if !pending.is_empty() {
            return Err("CLM checkpoint contains unsettled tool calls".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ModelToolCall;

    fn messages() -> Vec<ModelMessage> {
        vec![
            ModelMessage::User {
                content: "original task".into(),
            },
            ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![ModelToolCall {
                    call_id: "provider_call".into(),
                    tool_name: "read".into(),
                    arguments_json: r#"{"path":"task.md"}"#.into(),
                }],
            },
            ModelMessage::Tool {
                call_id: "provider_call".into(),
                tool_name: "read".into(),
                result: "private model observation\r\nsecond line".into(),
                metadata: serde_json::json!({"tool_name":"read","tool_metadata":{"offset":0}}),
            },
        ]
    }

    #[test]
    fn clm_checkpoint_round_trip_restores_native_tool_metadata_and_exact_text() {
        let original = messages();
        let checkpoint = ClmCheckpoint::from_messages(&original, 1).unwrap();
        let encoded = serde_json::to_string(&checkpoint).unwrap();
        let restored: ClmCheckpoint = serde_json::from_str(&encoded).unwrap();
        let hydrated = restored.messages().unwrap();
        assert_eq!(
            serde_json::to_value(&hydrated).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(restored.protected_prefix_len(), 1);
        let ModelMessage::Tool {
            metadata, result, ..
        } = &hydrated[2]
        else {
            panic!("tool")
        };
        assert_eq!(
            metadata,
            &serde_json::json!({"tool_name":"read","tool_metadata":{"offset":0}})
        );
        assert!(result.contains("\r\n"));
        assert!(!format!("{restored:?}").contains("private model observation"));
    }

    #[test]
    fn clm_checkpoint_metadata_survives_value_and_tagged_history_round_trips() {
        let checkpoint = ClmCheckpoint::from_messages(&messages(), 1).unwrap();
        let expected = serde_json::to_value(&checkpoint).unwrap();
        assert!(expected["tool_metadata"].get("2").is_some());
        let from_value: ClmCheckpoint = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(&from_value).unwrap(), expected);

        let history = crate::protocol::HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::Automatic,
            layout: crate::protocol::CompactionLayout::ClmCheckpoint,
            clm_checkpoint: Some(checkpoint),
            preserved_user_messages: Vec::new(),
            summary: "Context edited".into(),
            replacement_item_ids: Vec::new(),
        };
        let value = serde_json::to_value(&history).unwrap();
        let encoded = serde_json::to_string(&history).unwrap();
        for restored in [
            serde_json::from_value::<crate::protocol::HistoryItemPayload>(value.clone()).unwrap(),
            serde_json::from_str::<crate::protocol::HistoryItemPayload>(&encoded).unwrap(),
        ] {
            restored.validate_compaction_checkpoint().unwrap();
            assert_eq!(serde_json::to_value(&restored).unwrap(), value);
            let crate::protocol::HistoryItemPayload::Compaction {
                clm_checkpoint: Some(checkpoint),
                ..
            } = restored
            else {
                panic!("typed checkpoint");
            };
            assert_eq!(checkpoint.protected_prefix_len(), 1);
            let hydrated = checkpoint.messages().unwrap();
            let ModelMessage::Tool {
                metadata, result, ..
            } = &hydrated[2]
            else {
                panic!("native tool output");
            };
            assert_eq!(metadata, &expected["tool_metadata"]["2"]);
            assert_eq!(result, "private model observation\r\nsecond line");
        }
    }

    #[test]
    fn clm_checkpoint_rejects_invalid_and_duplicate_metadata_keys() {
        let valid =
            serde_json::to_value(ClmCheckpoint::from_messages(&messages(), 1).unwrap()).unwrap();
        for key in [
            "",
            "-1",
            "+2",
            "02",
            "2.0",
            "not-an-index",
            "184467440737095516160",
            "0",
            "3",
        ] {
            let mut modified = valid.clone();
            modified["tool_metadata"] = serde_json::json!({key: {"fixture": true}});
            assert!(
                serde_json::from_value::<ClmCheckpoint>(modified).is_err(),
                "{key}"
            );
        }
        let encoded_messages = serde_json::to_string(&messages()).unwrap();
        let duplicate = format!(
            r#"{{"version":1,"messages":{encoded_messages},"tool_metadata":{{"2":{{"first":true}},"2":{{"second":true}}}},"protected_prefix_len":1}}"#,
        );
        assert!(serde_json::from_str::<ClmCheckpoint>(&duplicate).is_err());
        let outer = format!(
            r#"{{"kind":"compaction","mode":"automatic","layout":"clm_checkpoint","clm_checkpoint":{duplicate},"preserved_user_messages":[],"summary":"Context edited","replacement_item_ids":[]}}"#,
        );
        assert!(serde_json::from_str::<crate::protocol::HistoryItemPayload>(&outer).is_err());
    }

    #[test]
    fn clm_checkpoint_rejects_authority_and_incomplete_native_pairs() {
        assert!(ClmCheckpoint::from_messages(&messages()[..2], 1).is_err());
        assert!(ClmCheckpoint::from_messages(&messages()[2..], 0).is_err());
        assert!(
            ClmCheckpoint::from_messages(
                &[ModelMessage::Developer {
                    content: "authority".into()
                }],
                1
            )
            .is_err()
        );
        let mut wrong_name = messages();
        if let ModelMessage::Tool { tool_name, .. } = &mut wrong_name[2] {
            *tool_name = "shell".into();
        }
        assert!(ClmCheckpoint::from_messages(&wrong_name, 1).is_err());
    }

    #[test]
    fn clm_checkpoint_rejects_invalid_persisted_shape() {
        let value =
            serde_json::to_value(ClmCheckpoint::from_messages(&messages(), 1).unwrap()).unwrap();
        for (field, invalid) in [
            ("version", serde_json::json!(2)),
            ("protected_prefix_len", serde_json::json!(4)),
            ("tool_metadata", serde_json::json!({"0": {"wrong":"owner"}})),
            ("tool_metadata", serde_json::json!({"3": {"wrong":"index"}})),
        ] {
            let mut modified = value.clone();
            modified[field] = invalid;
            assert!(
                serde_json::from_value::<ClmCheckpoint>(modified).is_err(),
                "{field}"
            );
        }
    }
}
