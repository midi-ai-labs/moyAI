use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::LlmError;
use crate::llm::{ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary};
use tokio_util::sync::CancellationToken;

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

impl PendingToolCall {
    fn is_complete(&self) -> bool {
        !self.tool_name.trim().is_empty() && !self.arguments_json.trim().is_empty()
    }
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
                            .filter(|pending| pending.is_complete())
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

pub(crate) async fn stream_chat_with_optional_terminal_timeout(
    llm: &Arc<dyn LlmClient>,
    request: ChatRequest,
    cancel: CancellationToken,
    sink: &mut dyn LlmEventSink,
    terminal_response_timeout_ms: Option<u64>,
) -> Result<LlmResponseSummary, LlmError> {
    let request_future = llm.stream_chat(request, cancel, sink);
    let Some(timeout_ms) = terminal_response_timeout_ms else {
        return request_future.await;
    };
    if timeout_ms == 0 {
        return request_future.await;
    }
    match tokio::time::timeout(Duration::from_millis(timeout_ms), request_future).await {
        Ok(result) => result,
        Err(_) => Err(LlmError::Message(provider_request_timeout_error_message(
            timeout_ms,
        ))),
    }
}

pub(crate) fn provider_request_timeout_error_message(timeout_ms: u64) -> String {
    format!("provider request timeout after {timeout_ms}ms before a terminal model response")
}

pub(crate) fn stream_accumulator_complete_tool_call_lifecycle_fixture_passes() -> bool {
    let mut args_only = StreamAccumulator::default();
    let args_only_ok = args_only
        .push(LlmEvent::ToolCallArgsDelta {
            call_id: "call_args_only".to_string(),
            delta: "{\"path\":\"src/workflow.rs\"}".to_string(),
        })
        .and_then(|_| {
            args_only.push(LlmEvent::Finished {
                finish_reason: crate::session::FinishReason::ToolCall,
                usage: None,
            })
        })
        .is_ok()
        && args_only.tool_calls.is_empty();

    let mut name_only = StreamAccumulator::default();
    let name_only_ok = name_only
        .push(LlmEvent::ToolCallStart {
            call_id: "call_name_only".to_string(),
            tool_name: "write".to_string(),
        })
        .and_then(|_| {
            name_only.push(LlmEvent::Finished {
                finish_reason: crate::session::FinishReason::ToolCall,
                usage: None,
            })
        })
        .is_ok()
        && name_only.tool_calls.is_empty();

    let mut reordered = StreamAccumulator::default();
    let reordered_ok = reordered
        .push(LlmEvent::ToolCallArgsDelta {
            call_id: "call_reordered".to_string(),
            delta: "{\"path\":\"src/workflow.rs\"}".to_string(),
        })
        .and_then(|_| {
            reordered.push(LlmEvent::ToolCallStart {
                call_id: "call_reordered".to_string(),
                tool_name: "write".to_string(),
            })
        })
        .and_then(|_| {
            reordered.push(LlmEvent::Finished {
                finish_reason: crate::session::FinishReason::ToolCall,
                usage: None,
            })
        })
        .is_ok()
        && reordered.tool_calls.len() == 1
        && reordered.tool_calls[0].tool_name == "write"
        && reordered.tool_calls[0].arguments_json == "{\"path\":\"src/workflow.rs\"}";

    args_only_ok && name_only_ok && reordered_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FinishReason;

    #[test]
    fn stream_accumulator_drops_args_only_incomplete_tool_call() {
        let mut accumulator = StreamAccumulator::default();
        accumulator
            .push(LlmEvent::ToolCallArgsDelta {
                call_id: "call_args_only".to_string(),
                delta: "{\"path\":\"src/workflow.rs\"}".to_string(),
            })
            .expect("args delta accepted as incomplete provider evidence");
        accumulator
            .push(LlmEvent::Finished {
                finish_reason: FinishReason::ToolCall,
                usage: None,
            })
            .expect("finish accepted");

        assert!(
            accumulator.tool_calls.is_empty(),
            "args-only provider stream evidence must not become a completed tool call"
        );
    }

    #[test]
    fn stream_accumulator_drops_name_only_incomplete_tool_call() {
        let mut accumulator = StreamAccumulator::default();
        accumulator
            .push(LlmEvent::ToolCallStart {
                call_id: "call_name_only".to_string(),
                tool_name: "write".to_string(),
            })
            .expect("tool call start accepted as incomplete provider evidence");
        accumulator
            .push(LlmEvent::Finished {
                finish_reason: FinishReason::ToolCall,
                usage: None,
            })
            .expect("finish accepted");

        assert!(
            accumulator.tool_calls.is_empty(),
            "name-only provider stream evidence must not become a completed tool call"
        );
    }

    #[test]
    fn stream_accumulator_completes_args_before_start_after_name_arrives() {
        let mut accumulator = StreamAccumulator::default();
        accumulator
            .push(LlmEvent::ToolCallArgsDelta {
                call_id: "call_reordered".to_string(),
                delta: "{\"path\":\"src/workflow.rs\"}".to_string(),
            })
            .expect("args delta accepted");
        accumulator
            .push(LlmEvent::ToolCallStart {
                call_id: "call_reordered".to_string(),
                tool_name: "write".to_string(),
            })
            .expect("tool call start accepted");
        accumulator
            .push(LlmEvent::Finished {
                finish_reason: FinishReason::ToolCall,
                usage: None,
            })
            .expect("finish accepted");

        assert_eq!(accumulator.tool_calls.len(), 1);
        assert_eq!(accumulator.tool_calls[0].tool_name, "write");
        assert_eq!(
            accumulator.tool_calls[0].arguments_json,
            "{\"path\":\"src/workflow.rs\"}"
        );
    }

    #[test]
    fn stream_accumulator_complete_tool_call_lifecycle_fixture() {
        assert!(stream_accumulator_complete_tool_call_lifecycle_fixture_passes());
    }
}
