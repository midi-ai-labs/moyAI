use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::config::model::{ProviderApiMode, ProviderReasoningCapability, ReasoningSummary};
use crate::error::LlmError;
use crate::llm::contract::{
    ChatRequest, LlmEvent, ModelContentPart, ModelMessage, ProviderToolChoice, ReasoningRequest,
    validate_responses_reasoning_request,
};
use crate::session::{FinishReason, TokenUsage};

/// Transport-only options for projecting a provider-neutral request onto the
/// Responses API. `input_start` indexes non-system `ModelMessage`s; expanding a
/// message into multiple Responses input items does not change that index.
#[derive(Debug, Clone, Copy)]
pub struct ResponsesRequestOptions<'a> {
    pub reasoning_request: Option<&'a ReasoningRequest>,
    pub reasoning_capability: ProviderReasoningCapability,
    pub previous_response_id: Option<&'a str>,
    pub input_start: usize,
}

impl Default for ResponsesRequestOptions<'_> {
    fn default() -> Self {
        Self {
            reasoning_request: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            previous_response_id: None,
            input_start: 0,
        }
    }
}

impl<'a> ResponsesRequestOptions<'a> {
    pub fn from_request(request: &'a ChatRequest) -> Self {
        let (previous_response_id, input_start) = request
            .responses_continuation
            .as_ref()
            .map(|continuation| {
                (
                    Some(continuation.previous_response_id.as_str()),
                    continuation.input_start,
                )
            })
            .unwrap_or((None, 0));

        Self {
            reasoning_request: request.reasoning.as_ref(),
            reasoning_capability: request.reasoning_capability,
            previous_response_id,
            input_start,
        }
    }
}

pub fn to_responses_request(
    request: &ChatRequest,
    options: ResponsesRequestOptions<'_>,
) -> Result<Value, LlmError> {
    request.validate_provider_lifecycle()?;
    if request.provider_target().api_mode() != ProviderApiMode::Responses {
        return Err(LlmError::Message(
            "Responses request serialization requires provider_api_mode=responses".to_string(),
        ));
    }

    let non_system_messages = request
        .messages
        .iter()
        .filter(|message| !matches!(message, ModelMessage::System { .. }))
        .collect::<Vec<_>>();
    if options.input_start > non_system_messages.len() {
        return Err(LlmError::Message(format!(
            "Responses input_start {} exceeds non-system message count {}",
            options.input_start,
            non_system_messages.len()
        )));
    }
    if options.input_start > 0 && options.previous_response_id.is_none() {
        return Err(LlmError::Message(
            "Responses input_start requires previous_response_id continuity".to_string(),
        ));
    }

    validate_previous_response_id(options)?;
    let reasoning = responses_reasoning(options)?;

    let mut input = Vec::new();
    for message in non_system_messages.into_iter().skip(options.input_start) {
        append_input_items(message, &mut input);
    }

    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();

    let tool_choice = match request.tool_choice.as_ref() {
        None => json!("auto"),
        Some(ProviderToolChoice::Required) => json!("required"),
        Some(ProviderToolChoice::Named { name }) => {
            json!({ "type": "function", "name": name })
        }
    };

    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.model.name));
    body.insert("instructions".to_string(), json!(instructions(request)));
    body.insert("input".to_string(), Value::Array(input));
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
        body.insert("tool_choice".to_string(), tool_choice);
        body.insert(
            "parallel_tool_calls".to_string(),
            json!(request.parallel_tool_calls),
        );
    }
    body.insert(
        "max_output_tokens".to_string(),
        json!(request.effective_max_output_tokens()),
    );
    body.insert("store".to_string(), Value::Bool(true));
    body.insert("stream".to_string(), Value::Bool(true));

    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    // LM Studio and other OpenAI-compatible providers may expose sampling
    // extensions beyond the standard Responses surface. Preserve explicit
    // request settings instead of silently dropping them when the transport
    // changes from Chat Completions to Responses.
    if let Some(top_k) = request.top_k {
        body.insert("top_k".to_string(), json!(top_k));
    }
    if let Some(presence_penalty) = request.presence_penalty {
        body.insert("presence_penalty".to_string(), json!(presence_penalty));
    }
    if let Some(frequency_penalty) = request.frequency_penalty {
        body.insert("frequency_penalty".to_string(), json!(frequency_penalty));
    }
    if let Some(seed) = request.seed {
        body.insert("seed".to_string(), json!(seed));
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop".to_string(), json!(request.stop_sequences));
    }

    if let Some(reasoning) = reasoning {
        body.insert("reasoning".to_string(), reasoning);
    }
    if let Some(previous_response_id) = options.previous_response_id {
        body.insert(
            "previous_response_id".to_string(),
            json!(previous_response_id),
        );
    }
    if let Some(extra) = &request.extra_body {
        merge_extra_body(&mut body, extra.clone());
    }

    Ok(Value::Object(body))
}

fn merge_extra_body(body: &mut Map<String, Value>, extra: Value) {
    match extra {
        Value::Object(extra) => {
            for (key, value) in extra {
                if !is_runtime_owned_responses_request_key(&key) {
                    body.insert(key, value);
                }
            }
        }
        value => {
            body.insert("extra_body_json".to_string(), value);
        }
    }
}

fn is_runtime_owned_responses_request_key(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "instructions"
            | "input"
            | "messages"
            | "tools"
            | "tool_choice"
            | "parallel_tool_calls"
            | "max_output_tokens"
            | "max_tokens"
            | "store"
            | "stream"
            | "temperature"
            | "top_p"
            | "top_k"
            | "presence_penalty"
            | "frequency_penalty"
            | "seed"
            | "stop"
            | "reasoning"
            | "reasoning_effort"
            | "reasoning_summary"
            | "previous_response_id"
    )
}

fn validate_previous_response_id(options: ResponsesRequestOptions<'_>) -> Result<(), LlmError> {
    let Some(previous_response_id) = options.previous_response_id else {
        return Ok(());
    };
    if previous_response_id.trim().is_empty() {
        return Err(LlmError::Message(
            "Responses previous_response_id must not be empty".to_string(),
        ));
    }
    if !matches!(
        options.reasoning_capability,
        ProviderReasoningCapability::Responses {
            supports_previous_response_id: true,
            ..
        }
    ) {
        return Err(LlmError::Message(
            "Responses continuation requires advertised previous_response_id support".to_string(),
        ));
    }
    Ok(())
}

fn responses_reasoning(options: ResponsesRequestOptions<'_>) -> Result<Option<Value>, LlmError> {
    let Some(reasoning) = validate_responses_reasoning_request(
        options.reasoning_request,
        options.reasoning_capability,
    )?
    else {
        return Ok(None);
    };

    let mut value = Map::new();
    if let Some(effort) = reasoning.effort {
        value.insert("effort".to_string(), json!(effort.as_str()));
    }
    if let Some(summary) = reasoning.summary {
        value.insert(
            "summary".to_string(),
            json!(reasoning_summary_name(summary)),
        );
    }
    Ok(Some(Value::Object(value)))
}

fn reasoning_summary_name(summary: ReasoningSummary) -> &'static str {
    match summary {
        ReasoningSummary::None => "none",
        ReasoningSummary::Auto => "auto",
        ReasoningSummary::Concise => "concise",
        ReasoningSummary::Detailed => "detailed",
    }
}

fn instructions(request: &ChatRequest) -> String {
    // Responses does not carry prior `instructions` forward with
    // `previous_response_id`, so current system/compaction guidance is projected
    // on every request instead of becoming another incremental input item.
    std::iter::once(request.system_prompt.clone())
        .chain(request.messages.iter().filter_map(|message| match message {
            ModelMessage::System { content } => Some(content.clone()),
            _ => None,
        }))
        .filter(|instruction| !instruction.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn append_input_items(message: &ModelMessage, input: &mut Vec<Value>) {
    match message {
        ModelMessage::System { .. } => {}
        ModelMessage::User { content } => input.push(message_item(
            "user",
            vec![json!({ "type": "input_text", "text": content })],
        )),
        ModelMessage::UserParts { parts } => {
            let content = parts
                .iter()
                .map(|part| match part {
                    ModelContentPart::Text { text } => {
                        json!({ "type": "input_text", "text": text })
                    }
                    ModelContentPart::Image {
                        mime_type,
                        data_base64,
                    } => json!({
                        "type": "input_image",
                        "image_url": format!("data:{mime_type};base64,{data_base64}"),
                    }),
                })
                .collect();
            input.push(message_item("user", content));
        }
        ModelMessage::Assistant { content } => input.push(message_item(
            "assistant",
            vec![json!({ "type": "output_text", "text": content })],
        )),
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => {
            if let Some(content) = content.as_deref().filter(|content| !content.is_empty()) {
                input.push(message_item(
                    "assistant",
                    vec![json!({ "type": "output_text", "text": content })],
                ));
            }
            input.extend(tool_calls.iter().map(|tool_call| {
                json!({
                    "type": "function_call",
                    "call_id": tool_call.call_id,
                    "name": tool_call.tool_name,
                    "arguments": tool_call.arguments_json,
                })
            }));
        }
        ModelMessage::Tool {
            call_id, result, ..
        } => input.push(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": result,
        })),
    }
}

fn message_item(role: &str, content: Vec<Value>) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

#[derive(Debug, Clone)]
pub enum ResponsesTerminal {
    Completed {
        response_id: String,
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
    Failed {
        response_id: Option<String>,
        code: Option<String>,
        message: String,
    },
    Incomplete {
        response_id: Option<String>,
        reason: String,
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ResponsesStreamUpdate {
    pub events: Vec<LlmEvent>,
    pub terminal: Option<ResponsesTerminal>,
}

#[derive(Debug, Clone)]
struct EmittedToolCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
pub struct ResponsesStreamAccumulator {
    pending_arguments: HashMap<String, String>,
    item_to_call: HashMap<String, String>,
    incomplete_function_items: HashSet<String>,
    emitted_tool_calls: HashMap<String, EmittedToolCall>,
    text_items_with_delta: HashSet<String>,
    completed_message_items: HashSet<String>,
    saw_unscoped_text_delta: bool,
    terminal: Option<ResponsesTerminal>,
}

impl ResponsesStreamAccumulator {
    pub fn terminal(&self) -> Option<&ResponsesTerminal> {
        self.terminal.as_ref()
    }

    pub fn push_json(&mut self, data: &str) -> Result<ResponsesStreamUpdate, LlmError> {
        let event = serde_json::from_str(data)?;
        self.push_value(&event)
    }

    pub fn push_value(&mut self, event: &Value) -> Result<ResponsesStreamUpdate, LlmError> {
        if self.terminal.is_some() {
            return Err(LlmError::Message(
                "Responses stream emitted an event after its terminal event".to_string(),
            ));
        }

        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::Message("Responses event is missing `type`".to_string()))?;
        let mut events = Vec::new();
        let terminal = match event_type {
            "response.output_text.delta" => {
                self.handle_output_text_delta(event, &mut events)?;
                None
            }
            "response.reasoning_summary_text.delta" => {
                let delta = required_string(event, "delta", event_type)?;
                events.push(LlmEvent::ReasoningSummaryDelta(delta.to_string()));
                None
            }
            // Raw chain-of-thought content is neither user-visible nor persistent state.
            "response.reasoning_text.delta" | "response.reasoning_text.done" => None,
            "response.output_item.done" => {
                self.handle_output_item_done(event, &mut events)?;
                None
            }
            "response.output_item.added" => {
                self.handle_output_item_added(event)?;
                None
            }
            "response.function_call_arguments.done" => {
                self.handle_function_call_arguments_done(event, &mut events)?;
                None
            }
            "response.function_call_arguments.delta" => {
                self.handle_function_call_arguments_delta(event)?;
                None
            }
            "response.completed" => Some(self.completed_terminal(event, &mut events)?),
            "response.failed" => Some(self.failed_terminal(event, &mut events)?),
            "response.incomplete" => Some(self.incomplete_terminal(event, &mut events)?),
            "error" => Some(self.error_terminal(event)?),
            _ => None,
        };

        if let Some(terminal) = terminal {
            self.terminal = Some(terminal.clone());
            Ok(ResponsesStreamUpdate {
                events,
                terminal: Some(terminal),
            })
        } else {
            Ok(ResponsesStreamUpdate {
                events,
                terminal: None,
            })
        }
    }

    fn handle_output_text_delta(
        &mut self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let delta = required_string(event, "delta", "response.output_text.delta")?;
        if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
            if self.completed_message_items.contains(item_id) {
                return Ok(());
            }
            self.text_items_with_delta.insert(item_id.to_string());
        } else {
            if !self.completed_message_items.is_empty() {
                return Ok(());
            }
            self.saw_unscoped_text_delta = true;
        }
        events.push(LlmEvent::TextDelta(delta.to_string()));
        Ok(())
    }

    fn handle_output_item_done(
        &mut self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let item = event.get("item").ok_or_else(|| {
            LlmError::Message("response.output_item.done is missing `item`".to_string())
        })?;
        match item.get("type").and_then(Value::as_str) {
            Some("message") => self.handle_message_done(item, events),
            Some("function_call") => {
                self.handle_function_call_done(item, events)?;
                if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                    self.incomplete_function_items.remove(item_id);
                }
                Ok(())
            }
            // In particular, ignore reasoning items and their raw `reasoning_text` content.
            _ => Ok(()),
        }
    }

    fn handle_output_item_added(&mut self, event: &Value) -> Result<(), LlmError> {
        let item = event.get("item").ok_or_else(|| {
            LlmError::Message("response.output_item.added is missing `item`".to_string())
        })?;
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let item_id =
                required_nonempty_string(item, "id", "Responses function_call item added")?;
            self.incomplete_function_items.insert(item_id.to_string());
        }
        Ok(())
    }

    fn handle_message_done(
        &mut self,
        item: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let item_id = item.get("id").and_then(Value::as_str);
        if item_id.is_some_and(|item_id| self.completed_message_items.contains(item_id)) {
            return Ok(());
        }

        let had_delta = self.saw_unscoped_text_delta
            || item_id.is_some_and(|item_id| self.text_items_with_delta.contains(item_id));
        if !had_delta {
            let content = item
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    LlmError::Message(
                        "Responses message output item is missing `content`".to_string(),
                    )
                })?;
            events.extend(content.iter().filter_map(|part| {
                (part.get("type").and_then(Value::as_str) == Some("output_text"))
                    .then(|| part.get("text").and_then(Value::as_str))
                    .flatten()
                    .map(|text| LlmEvent::TextDelta(text.to_string()))
            }));
        }
        if let Some(item_id) = item_id {
            self.completed_message_items.insert(item_id.to_string());
        }
        Ok(())
    }

    fn handle_function_call_done(
        &mut self,
        item: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let call_id = required_nonempty_string(item, "call_id", "Responses function_call item")?;
        let name = required_nonempty_string(item, "name", "Responses function_call item")?;
        let item_id = item.get("id").and_then(Value::as_str);
        if let Some(item_id) = item_id {
            self.bind_item_to_call(item_id, call_id)?;
        }

        let inline_arguments = item.get("arguments").and_then(Value::as_str);
        let call_arguments = self.pending_arguments.get(call_id).map(String::as_str);
        let item_arguments = item_id
            .and_then(|item_id| self.pending_arguments.get(item_id))
            .map(String::as_str);
        let arguments = consistent_arguments(
            [inline_arguments, call_arguments, item_arguments],
            "Responses function_call item",
        )?
        .ok_or_else(|| {
            LlmError::Message("Responses function_call item is missing `arguments`".to_string())
        })?
        .to_string();

        self.emit_tool_call(call_id, name, &arguments, events)
    }

    fn handle_function_call_arguments_done(
        &mut self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let arguments =
            required_string(event, "arguments", "response.function_call_arguments.done")?;
        let item_id = event.get("item_id").and_then(Value::as_str);
        let explicit_call_id = event.get("call_id").and_then(Value::as_str);
        if item_id.is_none() && explicit_call_id.is_none() {
            return Err(LlmError::Message(
                "response.function_call_arguments.done requires `item_id` or `call_id`".to_string(),
            ));
        }

        if let Some(item_id) = item_id {
            self.remember_final_arguments(item_id, arguments)?;
        }
        if let Some(call_id) = explicit_call_id {
            self.remember_final_arguments(call_id, arguments)?;
            if let Some(item_id) = item_id {
                self.bind_item_to_call(item_id, call_id)?;
            }
        }

        let call_id = explicit_call_id
            .map(str::to_string)
            .or_else(|| item_id.and_then(|item_id| self.item_to_call.get(item_id).cloned()));
        if let Some(call_id) = call_id.as_deref() {
            if let Some(emitted) = self.emitted_tool_calls.get(call_id) {
                if emitted.arguments != arguments {
                    return Err(LlmError::Message(format!(
                        "Responses function call `{call_id}` completed with conflicting arguments"
                    )));
                }
                return Ok(());
            }
            if let Some(name) = event.get("name").and_then(Value::as_str) {
                self.emit_tool_call(call_id, name, arguments, events)?;
            }
        }
        Ok(())
    }

    fn handle_function_call_arguments_delta(&mut self, event: &Value) -> Result<(), LlmError> {
        let delta = required_string(event, "delta", "response.function_call_arguments.delta")?;
        let key = event
            .get("item_id")
            .and_then(Value::as_str)
            .or_else(|| event.get("call_id").and_then(Value::as_str))
            .ok_or_else(|| {
                LlmError::Message(
                    "response.function_call_arguments.delta requires `item_id` or `call_id`"
                        .to_string(),
                )
            })?;
        self.pending_arguments
            .entry(key.to_string())
            .or_default()
            .push_str(delta);
        Ok(())
    }

    fn remember_final_arguments(&mut self, key: &str, arguments: &str) -> Result<(), LlmError> {
        if let Some(existing) = self.pending_arguments.get_mut(key) {
            if existing == arguments {
                return Ok(());
            }
            if arguments.starts_with(existing.as_str()) {
                *existing = arguments.to_string();
                return Ok(());
            }
            return Err(LlmError::Message(format!(
                "Responses function call item `{key}` completed with conflicting arguments"
            )));
        }
        self.pending_arguments
            .insert(key.to_string(), arguments.to_string());
        Ok(())
    }

    fn bind_item_to_call(&mut self, item_id: &str, call_id: &str) -> Result<(), LlmError> {
        if let Some(existing) = self.item_to_call.get(item_id) {
            if existing != call_id {
                return Err(LlmError::Message(format!(
                    "Responses item `{item_id}` was associated with multiple function calls"
                )));
            }
            return Ok(());
        }
        self.item_to_call
            .insert(item_id.to_string(), call_id.to_string());
        Ok(())
    }

    fn emit_tool_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        if let Some(existing) = self.emitted_tool_calls.get(call_id) {
            if existing.name != name || existing.arguments != arguments {
                return Err(LlmError::Message(format!(
                    "Responses function call `{call_id}` was emitted with conflicting content"
                )));
            }
            return Ok(());
        }

        self.remember_final_arguments(call_id, arguments)?;
        self.emitted_tool_calls.insert(
            call_id.to_string(),
            EmittedToolCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        );
        events.push(LlmEvent::ToolCallStart {
            call_id: call_id.to_string(),
            tool_name: name.to_string(),
        });
        events.push(LlmEvent::ToolCallArgsDelta {
            call_id: call_id.to_string(),
            delta: arguments.to_string(),
        });
        Ok(())
    }

    fn completed_terminal(
        &self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<ResponsesTerminal, LlmError> {
        self.validate_completed_function_calls()?;
        let response = response_object(event, "response.completed")?;
        let response_id = required_nonempty_string(response, "id", "response.completed response")?;
        let finish_reason = if self.emitted_tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCall
        };
        let usage = parse_usage(response);
        events.push(LlmEvent::Finished {
            finish_reason,
            usage: usage.clone(),
        });
        Ok(ResponsesTerminal::Completed {
            response_id: response_id.to_string(),
            finish_reason,
            usage,
        })
    }

    fn validate_completed_function_calls(&self) -> Result<(), LlmError> {
        if let Some(item_id) = self.incomplete_function_items.iter().next() {
            return Err(LlmError::Message(format!(
                "Responses stream completed before function call item `{item_id}` was complete"
            )));
        }
        if let Some((item_id, call_id)) = self
            .item_to_call
            .iter()
            .find(|(_, call_id)| !self.emitted_tool_calls.contains_key(call_id.as_str()))
        {
            return Err(LlmError::Message(format!(
                "Responses stream completed before function call `{call_id}` for item `{item_id}` was complete"
            )));
        }
        if let Some(key) = self.pending_arguments.keys().find(|key| {
            !self.emitted_tool_calls.contains_key(key.as_str())
                && !self
                    .item_to_call
                    .get(key.as_str())
                    .is_some_and(|call_id| self.emitted_tool_calls.contains_key(call_id))
        }) {
            return Err(LlmError::Message(format!(
                "Responses stream completed with unresolved function arguments for `{key}`"
            )));
        }
        Ok(())
    }

    fn failed_terminal(
        &self,
        event: &Value,
        _events: &mut Vec<LlmEvent>,
    ) -> Result<ResponsesTerminal, LlmError> {
        let response = response_object(event, "response.failed")?;
        let error = response.get("error");
        let code = error
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let message = error
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Responses request failed")
            .to_string();
        Ok(ResponsesTerminal::Failed {
            response_id: optional_response_id(response),
            code,
            message,
        })
    }

    fn incomplete_terminal(
        &self,
        event: &Value,
        _events: &mut Vec<LlmEvent>,
    ) -> Result<ResponsesTerminal, LlmError> {
        let response = response_object(event, "response.incomplete")?;
        let reason = response
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let finish_reason = match reason.as_str() {
            "max_output_tokens" | "max_tokens" => FinishReason::Length,
            _ => FinishReason::Error,
        };
        let usage = parse_usage(response);
        Ok(ResponsesTerminal::Incomplete {
            response_id: optional_response_id(response),
            reason,
            finish_reason,
            usage,
        })
    }

    fn error_terminal(&self, event: &Value) -> Result<ResponsesTerminal, LlmError> {
        let error = event.get("error").unwrap_or(event);
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_string);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Responses stream error")
            .to_string();
        Ok(ResponsesTerminal::Failed {
            response_id: None,
            code,
            message,
        })
    }
}

fn response_object<'a>(event: &'a Value, event_type: &str) -> Result<&'a Value, LlmError> {
    event
        .get("response")
        .filter(|response| response.is_object())
        .ok_or_else(|| LlmError::Message(format!("{event_type} is missing `response`")))
}

fn required_string<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str, LlmError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| LlmError::Message(format!("{context} is missing string field `{field}`")))
}

fn required_nonempty_string<'a>(
    value: &'a Value,
    field: &str,
    context: &str,
) -> Result<&'a str, LlmError> {
    required_string(value, field, context).and_then(|text| {
        (!text.trim().is_empty()).then_some(text).ok_or_else(|| {
            LlmError::Message(format!(
                "{context} contains an empty string field `{field}`"
            ))
        })
    })
}

fn consistent_arguments<'a, const N: usize>(
    candidates: [Option<&'a str>; N],
    context: &str,
) -> Result<Option<&'a str>, LlmError> {
    let candidates = candidates.into_iter().flatten().collect::<Vec<_>>();
    let Some(final_candidate) = candidates.iter().max_by_key(|candidate| candidate.len()) else {
        return Ok(None);
    };
    if candidates
        .iter()
        .any(|candidate| !final_candidate.starts_with(*candidate))
    {
        return Err(LlmError::Message(format!(
            "{context} contains conflicting function arguments"
        )));
    }
    Ok(Some(*final_candidate))
}

fn optional_response_id(response: &Value) -> Option<String> {
    response
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn parse_usage(response: &Value) -> Option<TokenUsage> {
    let usage = response.get("usage")?;
    let prompt_tokens = usage.get("input_tokens")?.as_u64()?;
    let completion_tokens = usage.get("output_tokens")?.as_u64()?;
    let total_tokens = usage.get("total_tokens")?.as_u64()?;
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .map(saturating_u32);
    Some(TokenUsage {
        prompt_tokens: saturating_u32(prompt_tokens),
        completion_tokens: saturating_u32(completion_tokens),
        total_tokens: saturating_u32(total_tokens),
        reasoning_tokens,
    })
}

fn saturating_u32(value: u64) -> u32 {
    value.min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::model::{ProviderApiMode, ReasoningEffort};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::llm::contract::{
        ModelCapabilities, ModelProfile, ModelToolCall, ResponsesContinuation, ToolSchema,
    };

    fn request(messages: Vec<ModelMessage>) -> ChatRequest {
        let model = ModelProfile {
            name: "gpt-test".to_string(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: true,
            },
        };
        let provider = ProviderTarget::new(
            "https://example.test/v1",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        let mut request = ChatRequest::new(
            provider,
            model,
            "Base instructions".to_string(),
            messages,
            vec![ToolSchema {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            }],
            None,
            responses_capability(),
            BTreeMap::new(),
        );
        request.tool_choice = Some(ProviderToolChoice::Named {
            name: "read_file".to_string(),
        });
        request.parallel_tool_calls = true;
        request
    }

    fn responses_capability() -> ProviderReasoningCapability {
        ProviderReasoningCapability::Responses {
            supports_summary: true,
            supports_previous_response_id: true,
        }
    }

    #[test]
    fn request_uses_non_system_message_index_and_standard_wire_shapes() {
        let request = request(vec![
            ModelMessage::System {
                content: "Additional policy".to_string(),
            },
            ModelMessage::User {
                content: "Already represented".to_string(),
            },
            ModelMessage::AssistantToolCalls {
                content: Some("I will inspect it.".to_string()),
                tool_calls: vec![ModelToolCall {
                    call_id: "call_1".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_json: r#"{"path":"README.md"}"#.to_string(),
                }],
            },
            ModelMessage::Tool {
                call_id: "call_1".to_string(),
                tool_name: "read_file".to_string(),
                result: "contents".to_string(),
                metadata: Value::Null,
            },
        ]);
        let reasoning = ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Detailed,
        };

        let wire = to_responses_request(
            &request,
            ResponsesRequestOptions {
                reasoning_request: Some(&reasoning),
                reasoning_capability: responses_capability(),
                previous_response_id: Some("resp_previous"),
                input_start: 1,
            },
        )
        .expect("request should serialize");

        assert_eq!(
            wire["instructions"],
            json!("Base instructions\n\nAdditional policy")
        );
        assert_eq!(wire["previous_response_id"], json!("resp_previous"));
        assert_eq!(wire["store"], json!(true));
        assert_eq!(wire["stream"], json!(true));
        assert_eq!(wire["parallel_tool_calls"], json!(true));
        assert_eq!(wire["max_output_tokens"], json!(4_096));
        assert_eq!(
            wire["reasoning"],
            json!({ "effort": "high", "summary": "detailed" })
        );
        assert_eq!(
            wire["tool_choice"],
            json!({ "type": "function", "name": "read_file" })
        );
        assert_eq!(wire["tools"][0]["type"], json!("function"));
        assert_eq!(wire["tools"][0]["parameters"]["type"], json!("object"));
        assert!(wire["tools"][0].get("strict").is_none());

        let input = wire["input"].as_array().expect("input array");
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], json!("message"));
        assert_eq!(input[0]["role"], json!("assistant"));
        assert_eq!(input[1]["type"], json!("function_call"));
        assert_eq!(input[1]["call_id"], json!("call_1"));
        assert_eq!(input[2]["type"], json!("function_call_output"));
        assert_eq!(input[2]["output"], json!("contents"));
    }

    #[test]
    fn request_preserves_sampling_and_extra_body_without_overriding_runtime_fields() {
        let mut request = request(vec![ModelMessage::User {
            content: "Inspect the repository".to_string(),
        }]);
        request.temperature = Some(0.2);
        request.top_p = Some(0.8);
        request.top_k = Some(40);
        request.presence_penalty = Some(0.1);
        request.frequency_penalty = Some(0.3);
        request.seed = Some(7);
        request.stop_sequences = vec!["DONE".to_string(), "STOP".to_string()];
        request.extra_body = Some(json!({
            "num_ctx": 131_072,
            "min_p": 0.05,
            "model": "overridden",
            "input": "overridden",
            "previous_response_id": "overridden",
            "reasoning": { "effort": "overridden" },
            "temperature": 1.9,
            "stop": ["overridden"]
        }));
        let reasoning = ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Concise,
        };

        let wire = to_responses_request(
            &request,
            ResponsesRequestOptions {
                reasoning_request: Some(&reasoning),
                reasoning_capability: responses_capability(),
                previous_response_id: Some("resp_previous"),
                input_start: 0,
            },
        )
        .expect("request should preserve explicit provider settings");

        assert_eq!(wire["model"], json!("gpt-test"));
        assert_eq!(wire["input"][0]["role"], json!("user"));
        assert_eq!(wire["previous_response_id"], json!("resp_previous"));
        assert_eq!(
            wire["reasoning"],
            json!({ "effort": "high", "summary": "concise" })
        );
        assert_eq!(wire["temperature"], json!(0.2));
        assert_eq!(wire["top_p"], json!(0.8));
        assert_eq!(wire["top_k"], json!(40));
        assert_eq!(wire["presence_penalty"], json!(0.1));
        assert_eq!(wire["frequency_penalty"], json!(0.3));
        assert_eq!(wire["seed"], json!(7));
        assert_eq!(wire["stop"], json!(["DONE", "STOP"]));
        assert_eq!(wire["num_ctx"], json!(131_072));
        assert_eq!(wire["min_p"], json!(0.05));
    }

    #[test]
    fn request_projects_text_and_image_parts() {
        let mut request = request(vec![ModelMessage::UserParts {
            parts: vec![
                ModelContentPart::Text {
                    text: "inspect".to_string(),
                },
                ModelContentPart::Image {
                    mime_type: "image/png".to_string(),
                    data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".to_string(),
                },
            ],
        }]);
        request.tool_choice = None;

        let wire = to_responses_request(
            &request,
            ResponsesRequestOptions {
                reasoning_capability: responses_capability(),
                ..ResponsesRequestOptions::default()
            },
        )
        .expect("request should serialize");

        assert_eq!(wire["tool_choice"], json!("auto"));
        assert_eq!(wire["input"][0]["content"][0]["type"], json!("input_text"));
        assert_eq!(
            wire["input"][0]["content"][1]["image_url"],
            json!(
                "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            )
        );
    }

    #[test]
    fn request_rejects_invalid_continuation_and_reasoning_capability() {
        let request = request(vec![ModelMessage::System {
            content: "Only system".to_string(),
        }]);
        let no_previous = to_responses_request(
            &request,
            ResponsesRequestOptions {
                input_start: 1,
                reasoning_capability: responses_capability(),
                ..ResponsesRequestOptions::default()
            },
        );
        assert!(no_previous.is_err());

        let reasoning = ReasoningRequest {
            effort: Some(ReasoningEffort::Medium),
            summary: ReasoningSummary::None,
        };
        let wrong_capability = to_responses_request(
            &request,
            ResponsesRequestOptions {
                reasoning_request: Some(&reasoning),
                reasoning_capability: ProviderReasoningCapability::Unsupported,
                ..ResponsesRequestOptions::default()
            },
        );
        assert!(wrong_capability.is_err());
    }

    #[test]
    fn request_options_can_be_derived_from_chat_request() {
        let mut request = request(vec![ModelMessage::User {
            content: "new input".to_string(),
        }]);
        request.reasoning = Some(ReasoningRequest {
            effort: Some(ReasoningEffort::Low),
            summary: ReasoningSummary::Concise,
        });
        request.responses_continuation = Some(ResponsesContinuation {
            previous_response_id: "resp_1".to_string(),
            input_start: 0,
        });

        let options = ResponsesRequestOptions::from_request(&request);
        assert_eq!(options.previous_response_id, Some("resp_1"));
        assert_eq!(options.input_start, 0);
        assert_eq!(options.reasoning_request, request.reasoning.as_ref());
    }

    #[test]
    fn output_text_and_reasoning_summary_are_typed_without_raw_reasoning() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let text = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "delta": "hello"
            }))
            .expect("text delta");
        assert!(matches!(
            text.events.as_slice(),
            [LlmEvent::TextDelta(delta)] if delta == "hello"
        ));

        let summary = accumulator
            .push_value(&json!({
                "type": "response.reasoning_summary_text.delta",
                "delta": "Checked the files"
            }))
            .expect("reasoning summary");
        assert!(matches!(
            summary.events.as_slice(),
            [LlmEvent::ReasoningSummaryDelta(delta)] if delta == "Checked the files"
        ));

        let raw = accumulator
            .push_value(&json!({
                "type": "response.reasoning_text.delta",
                "delta": "private chain of thought"
            }))
            .expect("raw reasoning is ignored");
        assert!(raw.events.is_empty());

        let done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hello" }]
                }
            }))
            .expect("message done");
        assert!(
            done.events.is_empty(),
            "full item must not duplicate deltas"
        );
    }

    #[test]
    fn message_done_emits_text_when_no_delta_was_seen() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let update = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "complete text" }]
                }
            }))
            .expect("message done");
        assert!(matches!(
            update.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "complete text"
        ));
    }

    #[test]
    fn completed_function_arguments_and_output_item_emit_one_tool_call() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        for delta in ["{\"path\":", "\"README.md\"}"] {
            let argument_delta = accumulator
                .push_value(&json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": "fc_1",
                    "delta": delta
                }))
                .expect("arguments delta");
            assert!(argument_delta.events.is_empty());
        }
        let arguments_done = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_1",
                "arguments": "{\"path\":\"README.md\"}"
            }))
            .expect("arguments done");
        assert!(arguments_done.events.is_empty());

        let item_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            }))
            .expect("function call item");
        assert!(matches!(
            item_done.events.as_slice(),
            [
                LlmEvent::ToolCallStart { call_id, tool_name },
                LlmEvent::ToolCallArgsDelta { call_id: args_id, delta }
            ] if call_id == "call_1"
                && tool_name == "read_file"
                && args_id == "call_1"
                && delta == "{\"path\":\"README.md\"}"
        ));

        let duplicate_arguments = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_1",
                "arguments": "{\"path\":\"README.md\"}"
            }))
            .expect("duplicate arguments");
        assert!(duplicate_arguments.events.is_empty());

        let duplicate_item = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            }))
            .expect("duplicate item");
        assert!(duplicate_item.events.is_empty());

        let completed = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "usage": {
                        "input_tokens": 11,
                        "output_tokens": 7,
                        "total_tokens": 18,
                        "output_tokens_details": { "reasoning_tokens": 3 }
                    }
                }
            }))
            .expect("completed response");
        assert!(matches!(
            completed.events.as_slice(),
            [LlmEvent::Finished {
                finish_reason: FinishReason::ToolCall,
                usage: Some(TokenUsage {
                    prompt_tokens: 11,
                    completion_tokens: 7,
                    total_tokens: 18,
                    reasoning_tokens: Some(3),
                }),
            }]
        ));
        assert!(matches!(
            completed.terminal,
            Some(ResponsesTerminal::Completed {
                response_id,
                finish_reason: FinishReason::ToolCall,
                ..
            }) if response_id == "resp_1"
        ));
    }

    #[test]
    fn malformed_function_arguments_are_preserved_for_runtime_parsing() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let item_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": "fc_raw",
                    "call_id": "call_raw",
                    "name": "read_file",
                    "arguments": "{\"path\":"
                }
            }))
            .expect("transport must retain raw provider arguments");

        assert!(matches!(
            item_done.events.as_slice(),
            [
                LlmEvent::ToolCallStart { call_id, tool_name },
                LlmEvent::ToolCallArgsDelta { call_id: args_id, delta }
            ] if call_id == "call_raw"
                && tool_name == "read_file"
                && args_id == "call_raw"
                && delta == "{\"path\":"
        ));

        let completed = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_raw" }
            }))
            .expect("transport completion does not parse tool arguments");
        assert!(matches!(
            completed.terminal,
            Some(ResponsesTerminal::Completed {
                finish_reason: FinishReason::ToolCall,
                ..
            })
        ));
    }

    #[test]
    fn completed_response_rejects_unresolved_function_argument_state() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_unresolved",
                "delta": "{\"path\":"
            }))
            .expect("arguments delta");

        let error = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_invalid" }
            }))
            .expect_err("unresolved function state cannot become a stop terminal");

        assert!(error.to_string().contains("unresolved function arguments"));
        assert!(accumulator.terminal().is_none());

        let mut added_only = ResponsesStreamAccumulator::default();
        added_only
            .push_value(&json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "id": "fc_added_only",
                    "call_id": "call_added_only",
                    "name": "read_file"
                }
            }))
            .expect("function item added");
        let error = added_only
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_invalid_added" }
            }))
            .expect_err("an added function item requires a matching done event");
        assert!(error.to_string().contains("fc_added_only"));
        assert!(added_only.terminal().is_none());
    }

    #[test]
    fn failed_and_incomplete_events_have_typed_terminals() {
        let mut failed_accumulator = ResponsesStreamAccumulator::default();
        let failed = failed_accumulator
            .push_value(&json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_failed",
                    "error": { "code": "server_error", "message": "unavailable" }
                }
            }))
            .expect("failed response");
        assert!(matches!(
            failed.terminal,
            Some(ResponsesTerminal::Failed {
                response_id: Some(response_id),
                code: Some(code),
                message,
            }) if response_id == "resp_failed" && code == "server_error" && message == "unavailable"
        ));
        assert!(failed.events.is_empty());

        let mut incomplete_accumulator = ResponsesStreamAccumulator::default();
        let incomplete = incomplete_accumulator
            .push_value(&json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_incomplete",
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 20,
                        "total_tokens": 30
                    }
                }
            }))
            .expect("incomplete response");
        assert!(incomplete.events.is_empty());
        assert!(matches!(
            incomplete.terminal,
            Some(ResponsesTerminal::Incomplete {
                reason,
                finish_reason: FinishReason::Length,
                ..
            }) if reason == "max_output_tokens"
        ));

        let mut error_accumulator = ResponsesStreamAccumulator::default();
        let error = error_accumulator
            .push_value(&json!({
                "type": "error",
                "error": { "code": "invalid_request", "message": "bad input" }
            }))
            .expect("top-level error");
        assert!(error.events.is_empty());
        assert!(matches!(
            error.terminal,
            Some(ResponsesTerminal::Failed {
                response_id: None,
                code: Some(code),
                message,
            }) if code == "invalid_request" && message == "bad input"
        ));
    }
}
