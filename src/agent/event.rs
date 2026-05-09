use std::collections::BTreeMap;

use crate::error::LlmError;
use crate::llm::{LlmEvent, LlmEventSink};

#[derive(Debug, Clone)]
pub struct CompletedToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
}

#[derive(Default)]
pub struct StreamAccumulator {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<CompletedToolCall>,
    finish_reason: Option<crate::session::FinishReason>,
    usage: Option<crate::session::TokenUsage>,
    pending_tool_calls: BTreeMap<String, PendingToolCall>,
    tool_call_order: Vec<String>,
}

#[derive(Default)]
struct PendingToolCall {
    tool_name: String,
    arguments_json: String,
}

impl StreamAccumulator {
    pub fn finish_reason(&self) -> Option<crate::session::FinishReason> {
        self.finish_reason
    }

    pub fn usage(&self) -> Option<crate::session::TokenUsage> {
        self.usage.clone()
    }
}

impl LlmEventSink for StreamAccumulator {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(value) => self.text.push_str(&value),
            LlmEvent::ReasoningDelta(value) => self.reasoning.push_str(&value),
            LlmEvent::ToolCallStart { call_id, tool_name } => {
                if !self.pending_tool_calls.contains_key(&call_id) {
                    self.tool_call_order.push(call_id.clone());
                }
                self.pending_tool_calls
                    .entry(call_id)
                    .or_default()
                    .tool_name = tool_name;
            }
            LlmEvent::ToolCallArgsDelta { call_id, delta } => {
                if !self.pending_tool_calls.contains_key(&call_id) {
                    self.tool_call_order.push(call_id.clone());
                }
                self.pending_tool_calls
                    .entry(call_id)
                    .or_default()
                    .arguments_json
                    .push_str(&delta);
            }
            LlmEvent::Finished {
                finish_reason,
                usage,
            } => {
                self.finish_reason = Some(finish_reason);
                self.usage = usage;
                self.tool_calls = self
                    .tool_call_order
                    .iter()
                    .filter_map(|call_id| {
                        self.pending_tool_calls
                            .get(call_id)
                            .map(|pending| CompletedToolCall {
                                call_id: call_id.clone(),
                                tool_name: pending.tool_name.clone(),
                                arguments_json: pending.arguments_json.clone(),
                            })
                    })
                    .collect();
            }
        }
        Ok(())
    }
}
