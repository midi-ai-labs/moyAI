use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::config::ProviderStreamLimits;
use crate::config::model::{ProviderApiMode, ProviderReasoningCapability, ReasoningSummary};
use crate::error::{LlmError, ProviderStreamLimit};
use crate::llm::contract::{
    ChatRequest, LlmEvent, ModelContentPart, ModelMessage, ProviderToolChoice, ReasoningRequest,
    validate_responses_reasoning_request,
};
use crate::session::{FinishReason, TokenUsage};

/// Transport-only options for projecting a provider-neutral request onto the
/// Responses API.
#[derive(Debug, Clone, Copy)]
pub struct ResponsesRequestOptions<'a> {
    pub reasoning_request: Option<&'a ReasoningRequest>,
    pub reasoning_capability: ProviderReasoningCapability,
}

impl Default for ResponsesRequestOptions<'_> {
    fn default() -> Self {
        Self {
            reasoning_request: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
        }
    }
}

impl<'a> ResponsesRequestOptions<'a> {
    pub fn from_request(request: &'a ChatRequest) -> Self {
        Self {
            reasoning_request: request.reasoning.as_ref(),
            reasoning_capability: request.reasoning_capability,
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

    let reasoning = responses_reasoning(options)?;

    let mut input = Vec::new();
    for message in request.messages.iter().filter(|message| {
        !matches!(
            message,
            ModelMessage::System { .. } | ModelMessage::Developer { .. }
        )
    }) {
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
    body.insert(
        "model".to_string(),
        json!(request.provider_target().model()),
    );
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
    body.insert("store".to_string(), Value::Bool(false));
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
    // HTTP Responses receives the complete canonical input on every request,
    // including the current system guidance.
    std::iter::once(request.system_prompt.clone())
        .chain(request.messages.iter().filter_map(|message| match message {
            ModelMessage::System { content } | ModelMessage::Developer { content } => {
                Some(content.clone())
            }
            _ => None,
        }))
        .filter(|instruction| !instruction.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn append_input_items(message: &ModelMessage, input: &mut Vec<Value>) {
    match message {
        ModelMessage::System { .. } | ModelMessage::Developer { .. } => {}
        ModelMessage::Agent { content } => input.push(message_item(
            "user",
            vec![json!({ "type": "input_text", "text": content })],
        )),
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FunctionArgumentKey {
    Item(String),
    Call(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResponseOutputItemKind {
    Message,
    FunctionCall,
    Reasoning,
}

impl ResponseOutputItemKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::FunctionCall => "function_call",
            Self::Reasoning => "reasoning",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResponseOutputIdentityRegistry {
    item_to_output: HashMap<String, (u64, ResponseOutputItemKind)>,
    output_index_to_item: HashMap<u64, (String, ResponseOutputItemKind)>,
}

impl ResponseOutputIdentityRegistry {
    fn ensure_binding(
        &self,
        item_id: &str,
        output_index: u64,
        kind: ResponseOutputItemKind,
    ) -> Result<(), LlmError> {
        if let Some((existing_index, existing_kind)) = self.item_to_output.get(item_id) {
            if *existing_index != output_index || *existing_kind != kind {
                return Err(LlmError::Message(format!(
                    "Responses output item `{item_id}` changed identity from {} at index `{existing_index}` to {} at index `{output_index}`",
                    existing_kind.as_str(),
                    kind.as_str(),
                )));
            }
        }
        if let Some((existing_item, existing_kind)) = self.output_index_to_item.get(&output_index) {
            if existing_item != item_id || *existing_kind != kind {
                return Err(LlmError::Message(format!(
                    "Responses output index `{output_index}` changed identity from {} item `{existing_item}` to {} item `{item_id}`",
                    existing_kind.as_str(),
                    kind.as_str(),
                )));
            }
        }
        Ok(())
    }

    fn bind(
        &mut self,
        item_id: &str,
        output_index: u64,
        kind: ResponseOutputItemKind,
    ) -> Result<(), LlmError> {
        self.ensure_binding(item_id, output_index, kind)?;
        self.item_to_output
            .entry(item_id.to_string())
            .or_insert((output_index, kind));
        self.output_index_to_item
            .entry(output_index)
            .or_insert_with(|| (item_id.to_string(), kind));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageTextLifecycle {
    Streaming,
    TextDone,
    ItemComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageTextState {
    text: String,
    lifecycle: MessageTextLifecycle,
}

impl MessageTextState {
    fn incomplete() -> Self {
        Self {
            text: String::new(),
            lifecycle: MessageTextLifecycle::Streaming,
        }
    }

    fn text_done(text: String) -> Self {
        Self {
            text,
            lifecycle: MessageTextLifecycle::TextDone,
        }
    }

    fn complete(text: String) -> Self {
        Self {
            text,
            lifecycle: MessageTextLifecycle::ItemComplete,
        }
    }

    fn is_complete(&self) -> bool {
        self.lifecycle == MessageTextLifecycle::ItemComplete
    }

    fn is_text_finalized(&self) -> bool {
        self.lifecycle != MessageTextLifecycle::Streaming
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FunctionItemLifecycle {
    Added,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionItemState {
    output_index: u64,
    call_id: String,
    name: String,
    lifecycle: FunctionItemLifecycle,
}

impl FunctionArgumentKey {
    fn item(item_id: &str) -> Self {
        Self::Item(item_id.to_string())
    }

    fn call(call_id: &str) -> Self {
        Self::Call(call_id.to_string())
    }

    fn description(&self) -> String {
        match self {
            Self::Item(item_id) => format!("item `{item_id}`"),
            Self::Call(call_id) => format!("call `{call_id}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FunctionArgumentLifecycle {
    Accumulating,
    Finalized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionArgumentState {
    value: String,
    lifecycle: FunctionArgumentLifecycle,
}

impl FunctionArgumentState {
    fn accumulating() -> Self {
        Self {
            value: String::new(),
            lifecycle: FunctionArgumentLifecycle::Accumulating,
        }
    }

    fn finalized(value: &str) -> Self {
        Self {
            value: value.to_string(),
            lifecycle: FunctionArgumentLifecycle::Finalized,
        }
    }

    fn len(&self) -> usize {
        self.value.len()
    }

    fn as_str(&self) -> &str {
        &self.value
    }

    fn is_finalized(&self) -> bool {
        self.lifecycle == FunctionArgumentLifecycle::Finalized
    }
}

#[derive(Debug, Clone, Copy)]
enum PendingArgumentMerge {
    None,
    KeepCall {
        length: usize,
        lifecycle: FunctionArgumentLifecycle,
    },
    MoveItem {
        length: usize,
        lifecycle: FunctionArgumentLifecycle,
    },
}

impl PendingArgumentMerge {
    fn length(self) -> usize {
        match self {
            Self::None => 0,
            Self::KeepCall { length, .. } | Self::MoveItem { length, .. } => length,
        }
    }

    fn lifecycle(self) -> FunctionArgumentLifecycle {
        match self {
            Self::None => FunctionArgumentLifecycle::Accumulating,
            Self::KeepCall { lifecycle, .. } | Self::MoveItem { lifecycle, .. } => lifecycle,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResponsesStreamAccumulator {
    max_tool_call_argument_bytes: u64,
    pending_arguments: HashMap<FunctionArgumentKey, FunctionArgumentState>,
    item_to_call: HashMap<String, String>,
    call_to_item: HashMap<String, String>,
    function_items: HashMap<String, FunctionItemState>,
    incomplete_reasoning_items: HashSet<String>,
    completed_reasoning_items: HashSet<String>,
    emitted_tool_calls: HashMap<String, EmittedToolCall>,
    output_identities: ResponseOutputIdentityRegistry,
    message_text_items: HashMap<String, MessageTextState>,
    terminal: Option<ResponsesTerminal>,
}

impl Default for ResponsesStreamAccumulator {
    fn default() -> Self {
        Self::new(ProviderStreamLimits::product_default().max_tool_call_argument_bytes)
    }
}

impl ResponsesStreamAccumulator {
    pub fn new(max_tool_call_argument_bytes: u64) -> Self {
        Self {
            max_tool_call_argument_bytes,
            pending_arguments: HashMap::new(),
            item_to_call: HashMap::new(),
            call_to_item: HashMap::new(),
            function_items: HashMap::new(),
            incomplete_reasoning_items: HashSet::new(),
            completed_reasoning_items: HashSet::new(),
            emitted_tool_calls: HashMap::new(),
            output_identities: ResponseOutputIdentityRegistry::default(),
            message_text_items: HashMap::new(),
            terminal: None,
        }
    }

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
            "response.output_text.done" => {
                self.handle_output_text_done(event, &mut events)?;
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
                self.apply_atomically(&mut events, |accumulator, _events| {
                    accumulator.handle_output_item_added(event)
                })?;
                None
            }
            "response.function_call_arguments.done" => {
                self.apply_atomically(&mut events, |accumulator, events| {
                    accumulator.handle_function_call_arguments_done(event, events)
                })?;
                None
            }
            "response.function_call_arguments.delta" => {
                self.handle_function_call_arguments_delta(event)?;
                None
            }
            "response.completed" => {
                Some(self.apply_atomically(&mut events, |accumulator, events| {
                    accumulator.completed_terminal(event, events)
                })?)
            }
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
        let context = "response.output_text.delta";
        let delta = required_string(event, "delta", context)?;
        let item_id = required_nonempty_string(event, "item_id", context)?;
        let output_index = required_u64(event, "output_index", context)?;
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::Message,
        )?;
        if self
            .message_text_items
            .get(item_id)
            .is_some_and(MessageTextState::is_text_finalized)
        {
            return Err(LlmError::Message(format!(
                "Responses message item `{item_id}` received a text delta after its text was finalized"
            )));
        }
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::Message)?;
        self.message_text_items
            .entry(item_id.to_string())
            .or_insert_with(MessageTextState::incomplete)
            .text
            .push_str(delta);
        events.push(LlmEvent::TextDelta(delta.to_string()));
        Ok(())
    }

    fn handle_output_text_done(
        &mut self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let context = "response.output_text.done";
        let text = required_string(event, "text", context)?;
        let item_id = required_nonempty_string(event, "item_id", context)?;
        let output_index = required_u64(event, "output_index", context)?;
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::Message,
        )?;

        if let Some(state) = self.message_text_items.get(item_id) {
            match state.lifecycle {
                MessageTextLifecycle::ItemComplete => {
                    return Err(LlmError::Message(format!(
                        "Responses message item `{item_id}` received output_text.done after item completion"
                    )));
                }
                MessageTextLifecycle::TextDone => {
                    if state.text == text {
                        return Ok(());
                    }
                    return Err(LlmError::Message(format!(
                        "Responses message item `{item_id}` received conflicting output_text.done values"
                    )));
                }
                MessageTextLifecycle::Streaming => {}
            }
        }

        let accumulated = self
            .message_text_items
            .get(item_id)
            .map(|state| state.text.as_str())
            .unwrap_or_default();
        let missing_suffix = text.strip_prefix(accumulated).ok_or_else(|| {
            LlmError::Message(format!(
                "Responses message item `{item_id}` finalized text that conflicts with its streamed deltas"
            ))
        })?;
        let missing_suffix = missing_suffix.to_string();
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::Message)?;
        self.message_text_items.insert(
            item_id.to_string(),
            MessageTextState::text_done(text.to_string()),
        );
        if !missing_suffix.is_empty() {
            events.push(LlmEvent::TextDelta(missing_suffix));
        }
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
            Some("message") => self.handle_message_done(event, item, events),
            Some("function_call") => {
                let item_id = required_nonempty_string(item, "id", "Responses function_call item")?;
                let output_index = required_u64(
                    event,
                    "output_index",
                    "Responses function_call output item done",
                )?;
                self.apply_atomically(events, |accumulator, events| {
                    accumulator.handle_function_call_done(item, item_id, output_index, events)
                })
            }
            Some("reasoning") => self.handle_reasoning_item_done(event, item),
            Some(item_type) => Err(LlmError::Message(format!(
                "response.output_item.done contains unsupported output item type `{item_type}`"
            ))),
            None => Err(LlmError::Message(
                "response.output_item.done item is missing string field `type`".to_string(),
            )),
        }
    }

    fn handle_output_item_added(&mut self, event: &Value) -> Result<(), LlmError> {
        let item = event.get("item").ok_or_else(|| {
            LlmError::Message("response.output_item.added is missing `item`".to_string())
        })?;
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let context = "Responses message output item added";
                let item_id = required_nonempty_string(item, "id", context)?;
                let output_index = required_u64(event, "output_index", context)?;
                validate_assistant_message_role(item, context)?;
                validate_optional_message_output_text(item, context)?;
                self.output_identities.ensure_binding(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::Message,
                )?;
                if self
                    .message_text_items
                    .get(item_id)
                    .is_some_and(MessageTextState::is_text_finalized)
                {
                    return Err(LlmError::Message(format!(
                        "Responses message item `{item_id}` was added after its text was finalized"
                    )));
                }
                self.output_identities.bind(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::Message,
                )?;
                self.message_text_items
                    .entry(item_id.to_string())
                    .or_insert_with(MessageTextState::incomplete);
            }
            Some("function_call") => {
                let context = "Responses function_call item added";
                let item_id = required_nonempty_string(item, "id", context)?;
                let output_index = required_u64(event, "output_index", context)?;
                let call_id = required_nonempty_string(item, "call_id", context)?;
                let name = required_nonempty_string(item, "name", context)?;
                if item.get("arguments").is_some() {
                    let arguments = required_string(item, "arguments", context)?;
                    self.ensure_argument_bytes(arguments.len())?;
                }
                self.output_identities.ensure_binding(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::FunctionCall,
                )?;
                if let Some(existing) = self.function_items.get(item_id) {
                    if existing.output_index != output_index
                        || existing.call_id != call_id
                        || existing.name != name
                    {
                        return Err(LlmError::Message(format!(
                            "Responses function item `{item_id}` changed identity between added events"
                        )));
                    }
                    if existing.lifecycle == FunctionItemLifecycle::Complete {
                        return Err(LlmError::Message(format!(
                            "Responses function item `{item_id}` was added after it was complete"
                        )));
                    }
                }
                self.bind_item_to_call(item_id, call_id)?;
                self.output_identities.bind(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::FunctionCall,
                )?;
                self.function_items
                    .entry(item_id.to_string())
                    .or_insert_with(|| FunctionItemState {
                        output_index,
                        call_id: call_id.to_string(),
                        name: name.to_string(),
                        lifecycle: FunctionItemLifecycle::Added,
                    });
            }
            Some("reasoning") => self.handle_reasoning_item_added(event, item)?,
            Some(item_type) => {
                return Err(LlmError::Message(format!(
                    "response.output_item.added contains unsupported output item type `{item_type}`"
                )));
            }
            None => {
                return Err(LlmError::Message(
                    "response.output_item.added item is missing string field `type`".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn handle_message_done(
        &mut self,
        event: &Value,
        item: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let context = "Responses message output item done";
        let output_index = required_u64(event, "output_index", context)?;
        self.handle_message_done_at_index(item, output_index, events)
    }

    fn handle_message_done_at_index(
        &mut self,
        item: &Value,
        output_index: u64,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let context = "Responses message output item done";
        let item_id = required_nonempty_string(item, "id", context)?;
        validate_assistant_message_role(item, context)?;
        let completed_text = required_message_output_text(item, context)?;
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::Message,
        )?;
        if let Some(state) = self
            .message_text_items
            .get(item_id)
            .filter(|state| state.is_complete())
        {
            if state.text == completed_text {
                return Ok(());
            }
            return Err(LlmError::Message(format!(
                "Responses message item `{item_id}` was completed with conflicting text"
            )));
        }

        if let Some(state) = self.message_text_items.get(item_id)
            && state.lifecycle == MessageTextLifecycle::TextDone
            && state.text != completed_text
        {
            return Err(LlmError::Message(format!(
                "Responses message item `{item_id}` item completion conflicts with output_text.done"
            )));
        }

        let accumulated = self
            .message_text_items
            .get(item_id)
            .map(|state| state.text.as_str())
            .unwrap_or_default();
        let missing_suffix = completed_text.strip_prefix(accumulated).ok_or_else(|| {
            LlmError::Message(format!(
                "Responses message item `{item_id}` completed with text that conflicts with its streamed deltas"
            ))
        })?;
        let missing_suffix = missing_suffix.to_string();
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::Message)?;
        self.message_text_items.insert(
            item_id.to_string(),
            MessageTextState::complete(completed_text),
        );
        if !missing_suffix.is_empty() {
            events.push(LlmEvent::TextDelta(missing_suffix));
        }
        Ok(())
    }

    fn handle_reasoning_item_added(&mut self, event: &Value, item: &Value) -> Result<(), LlmError> {
        let context = "Responses reasoning output item added";
        let item_id = required_nonempty_string(item, "id", context)?;
        let output_index = required_u64(event, "output_index", context)?;
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::Reasoning,
        )?;
        if self.completed_reasoning_items.contains(item_id) {
            return Err(LlmError::Message(format!(
                "Responses reasoning item `{item_id}` was added after it was complete"
            )));
        }
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::Reasoning)?;
        self.incomplete_reasoning_items.insert(item_id.to_string());
        Ok(())
    }

    fn handle_reasoning_item_done(&mut self, event: &Value, item: &Value) -> Result<(), LlmError> {
        let context = "Responses reasoning output item done";
        let item_id = required_nonempty_string(item, "id", context)?;
        let output_index = required_u64(event, "output_index", context)?;
        self.handle_reasoning_item_done_at_index(item_id, output_index)
    }

    fn handle_reasoning_item_done_at_index(
        &mut self,
        item_id: &str,
        output_index: u64,
    ) -> Result<(), LlmError> {
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::Reasoning,
        )?;
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::Reasoning)?;
        self.incomplete_reasoning_items.remove(item_id);
        self.completed_reasoning_items.insert(item_id.to_string());
        Ok(())
    }

    fn handle_function_call_done(
        &mut self,
        item: &Value,
        item_id: &str,
        output_index: u64,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let call_id = required_nonempty_string(item, "call_id", "Responses function_call item")?;
        let name = required_nonempty_string(item, "name", "Responses function_call item")?;
        let inline_arguments = item
            .get("arguments")
            .map(|_| required_string(item, "arguments", "Responses function_call item"))
            .transpose()?;
        if let Some(arguments) = inline_arguments {
            self.ensure_argument_bytes(arguments.len())?;
        }
        self.output_identities.ensure_binding(
            item_id,
            output_index,
            ResponseOutputItemKind::FunctionCall,
        )?;
        if let Some(existing) = self.function_items.get(item_id)
            && (existing.output_index != output_index
                || existing.call_id != call_id
                || existing.name != name)
        {
            return Err(LlmError::Message(format!(
                "Responses function item `{item_id}` changed identity between added and done"
            )));
        }
        self.bind_item_to_call(item_id, call_id)?;

        let call_arguments = self
            .pending_arguments
            .get(&FunctionArgumentKey::call(call_id))
            .map(FunctionArgumentState::as_str);
        let arguments = consistent_arguments(
            [inline_arguments, call_arguments],
            "Responses function_call item",
        )?
        .ok_or_else(|| {
            LlmError::Message("Responses function_call item is missing `arguments`".to_string())
        })?;
        self.ensure_argument_bytes(arguments.len())?;
        let arguments = arguments.to_string();

        self.emit_tool_call(call_id, name, &arguments, events)?;
        self.output_identities
            .bind(item_id, output_index, ResponseOutputItemKind::FunctionCall)?;
        self.function_items.insert(
            item_id.to_string(),
            FunctionItemState {
                output_index,
                call_id: call_id.to_string(),
                name: name.to_string(),
                lifecycle: FunctionItemLifecycle::Complete,
            },
        );
        Ok(())
    }

    fn handle_function_call_arguments_done(
        &mut self,
        event: &Value,
        _events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let context = "response.function_call_arguments.done";
        let arguments = required_string(event, "arguments", context)?;
        let item_id = event
            .get("item_id")
            .map(|_| required_nonempty_string(event, "item_id", context))
            .transpose()?;
        let explicit_call_id = event
            .get("call_id")
            .map(|_| required_nonempty_string(event, "call_id", context))
            .transpose()?;
        let output_index = event
            .get("output_index")
            .map(|_| required_u64(event, "output_index", context))
            .transpose()?;
        let name = event
            .get("name")
            .map(|_| required_nonempty_string(event, "name", context))
            .transpose()?;
        if item_id.is_none() && explicit_call_id.is_none() {
            return Err(LlmError::Message(
                "response.function_call_arguments.done requires `item_id` or `call_id`".to_string(),
            ));
        }
        self.ensure_argument_bytes(arguments.len())?;

        if let Some(item_id) = item_id {
            if let Some(existing) = self.function_items.get(item_id) {
                if output_index.is_some_and(|index| index != existing.output_index) {
                    return Err(LlmError::Message(format!(
                        "Responses function item `{item_id}` changed output_index during arguments.done"
                    )));
                }
                if explicit_call_id.is_some_and(|call_id| call_id != existing.call_id) {
                    return Err(LlmError::Message(format!(
                        "Responses function item `{item_id}` changed call_id during arguments.done"
                    )));
                }
                if name.is_some_and(|name| name != existing.name) {
                    return Err(LlmError::Message(format!(
                        "Responses function item `{item_id}` changed name during arguments.done"
                    )));
                }
            }
            if let Some(output_index) = output_index {
                self.output_identities.ensure_binding(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::FunctionCall,
                )?;
            }
        }

        if let (Some(item_id), Some(call_id)) = (item_id, explicit_call_id) {
            self.bind_item_to_call(item_id, call_id)?;
        }
        let argument_key = self
            .canonical_argument_key(item_id, explicit_call_id)
            .expect("an item or call id was validated above");
        self.remember_final_arguments(argument_key, arguments)?;

        if let (Some(item_id), Some(output_index)) = (item_id, output_index) {
            self.output_identities.bind(
                item_id,
                output_index,
                ResponseOutputItemKind::FunctionCall,
            )?;
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
                if name.is_some_and(|name| emitted.name != name) {
                    return Err(LlmError::Message(format!(
                        "Responses function call `{call_id}` completed with a conflicting name"
                    )));
                }
                return Ok(());
            }
        }
        Ok(())
    }

    fn handle_function_call_arguments_delta(&mut self, event: &Value) -> Result<(), LlmError> {
        let context = "response.function_call_arguments.delta";
        let delta = required_string(event, "delta", context)?;
        let item_id = event
            .get("item_id")
            .map(|_| required_nonempty_string(event, "item_id", context))
            .transpose()?;
        let call_id = event
            .get("call_id")
            .map(|_| required_nonempty_string(event, "call_id", context))
            .transpose()?;
        let output_index = event
            .get("output_index")
            .map(|_| required_u64(event, "output_index", context))
            .transpose()?;
        if item_id.is_none() && call_id.is_none() {
            return Err(LlmError::Message(
                "response.function_call_arguments.delta requires `item_id` or `call_id`"
                    .to_string(),
            ));
        }
        if let Some(item_id) = item_id {
            if let Some(existing) = self.function_items.get(item_id)
                && output_index.is_some_and(|index| index != existing.output_index)
            {
                return Err(LlmError::Message(format!(
                    "Responses function item `{item_id}` changed output_index during arguments.delta"
                )));
            }
            if let Some(output_index) = output_index {
                self.output_identities.ensure_binding(
                    item_id,
                    output_index,
                    ResponseOutputItemKind::FunctionCall,
                )?;
            }
        }

        let key = if let (Some(item_id), Some(call_id)) = (item_id, call_id) {
            let merge = self.plan_item_to_call_binding(item_id, call_id)?;
            let key = FunctionArgumentKey::call(call_id);
            self.ensure_argument_delta_allowed(&key, merge.lifecycle())?;
            self.ensure_argument_append(merge.length(), delta.len())?;
            self.apply_item_to_call_binding(item_id, call_id, merge);
            key
        } else {
            let key = self
                .canonical_argument_key(item_id, call_id)
                .expect("an item or call id was validated above");
            let current_state = self.pending_arguments.get(&key);
            let lifecycle = current_state
                .map_or(FunctionArgumentLifecycle::Accumulating, |state| {
                    state.lifecycle
                });
            self.ensure_argument_delta_allowed(&key, lifecycle)?;
            let current_length = current_state.map_or(0, FunctionArgumentState::len);
            self.ensure_argument_append(current_length, delta.len())?;
            key
        };
        self.pending_arguments
            .entry(key)
            .or_insert_with(FunctionArgumentState::accumulating)
            .value
            .push_str(delta);
        if let (Some(item_id), Some(output_index)) = (item_id, output_index) {
            self.output_identities.bind(
                item_id,
                output_index,
                ResponseOutputItemKind::FunctionCall,
            )?;
        }
        Ok(())
    }

    fn remember_final_arguments(
        &mut self,
        key: FunctionArgumentKey,
        arguments: &str,
    ) -> Result<(), LlmError> {
        self.ensure_argument_bytes(arguments.len())?;
        let key_description = key.description();
        if let Some(existing) = self.pending_arguments.get_mut(&key) {
            if existing.value == arguments {
                existing.lifecycle = FunctionArgumentLifecycle::Finalized;
                return Ok(());
            }
            if existing.is_finalized() {
                return Err(LlmError::Message(format!(
                    "Responses function call {key_description} was finalized with different arguments"
                )));
            }
            if arguments.starts_with(existing.value.as_str()) {
                existing.value = arguments.to_string();
                existing.lifecycle = FunctionArgumentLifecycle::Finalized;
                return Ok(());
            }
            return Err(LlmError::Message(format!(
                "Responses function call {key_description} completed with conflicting arguments"
            )));
        }
        self.pending_arguments
            .insert(key, FunctionArgumentState::finalized(arguments));
        Ok(())
    }

    fn bind_item_to_call(&mut self, item_id: &str, call_id: &str) -> Result<(), LlmError> {
        let merge = self.plan_item_to_call_binding(item_id, call_id)?;
        self.apply_item_to_call_binding(item_id, call_id, merge);
        Ok(())
    }

    fn plan_item_to_call_binding(
        &self,
        item_id: &str,
        call_id: &str,
    ) -> Result<PendingArgumentMerge, LlmError> {
        if let Some(existing) = self.item_to_call.get(item_id) {
            if existing != call_id {
                return Err(LlmError::Message(format!(
                    "Responses item `{item_id}` was associated with multiple function calls"
                )));
            }
        }
        if let Some(existing) = self.call_to_item.get(call_id) {
            if existing != item_id {
                return Err(LlmError::Message(format!(
                    "Responses function call `{call_id}` was associated with multiple items (`{existing}` and `{item_id}`)"
                )));
            }
        }

        let item_key = FunctionArgumentKey::item(item_id);
        let call_key = FunctionArgumentKey::call(call_id);
        let item_arguments = self.pending_arguments.get(&item_key);
        let call_arguments = self.pending_arguments.get(&call_key);
        match (item_arguments, call_arguments) {
            (None, None) => Ok(PendingArgumentMerge::None),
            (None, Some(call_arguments)) => Ok(PendingArgumentMerge::KeepCall {
                length: call_arguments.len(),
                lifecycle: call_arguments.lifecycle,
            }),
            (Some(item_arguments), None) => Ok(PendingArgumentMerge::MoveItem {
                length: item_arguments.len(),
                lifecycle: item_arguments.lifecycle,
            }),
            (Some(item_arguments), Some(call_arguments)) => {
                let (keep_call, canonical_arguments) = if call_arguments
                    .as_str()
                    .starts_with(item_arguments.as_str())
                {
                    (true, call_arguments)
                } else if item_arguments.as_str().starts_with(call_arguments.as_str()) {
                    (false, item_arguments)
                } else {
                    return Err(LlmError::Message(format!(
                        "Responses item `{item_id}` and function call `{call_id}` contain conflicting arguments"
                    )));
                };
                let lifecycle = Self::merged_argument_lifecycle(
                    item_arguments,
                    call_arguments,
                    canonical_arguments,
                    item_id,
                    call_id,
                )?;
                if keep_call {
                    Ok(PendingArgumentMerge::KeepCall {
                        length: canonical_arguments.len(),
                        lifecycle,
                    })
                } else {
                    Ok(PendingArgumentMerge::MoveItem {
                        length: canonical_arguments.len(),
                        lifecycle,
                    })
                }
            }
        }
    }

    fn merged_argument_lifecycle(
        item_arguments: &FunctionArgumentState,
        call_arguments: &FunctionArgumentState,
        canonical_arguments: &FunctionArgumentState,
        item_id: &str,
        call_id: &str,
    ) -> Result<FunctionArgumentLifecycle, LlmError> {
        for arguments in [item_arguments, call_arguments] {
            if arguments.is_finalized() && arguments.as_str() != canonical_arguments.as_str() {
                return Err(LlmError::Message(format!(
                    "Responses item `{item_id}` and function call `{call_id}` disagree with finalized arguments"
                )));
            }
        }
        Ok(
            if item_arguments.is_finalized() || call_arguments.is_finalized() {
                FunctionArgumentLifecycle::Finalized
            } else {
                FunctionArgumentLifecycle::Accumulating
            },
        )
    }

    fn apply_item_to_call_binding(
        &mut self,
        item_id: &str,
        call_id: &str,
        merge: PendingArgumentMerge,
    ) {
        self.item_to_call
            .entry(item_id.to_string())
            .or_insert_with(|| call_id.to_string());
        self.call_to_item
            .entry(call_id.to_string())
            .or_insert_with(|| item_id.to_string());
        let item_key = FunctionArgumentKey::item(item_id);
        let call_key = FunctionArgumentKey::call(call_id);
        match merge {
            PendingArgumentMerge::None => {}
            PendingArgumentMerge::KeepCall { lifecycle, .. } => {
                if let Some(arguments) = self.pending_arguments.get_mut(&call_key) {
                    arguments.lifecycle = lifecycle;
                }
                self.pending_arguments.remove(&item_key);
            }
            PendingArgumentMerge::MoveItem { lifecycle, .. } => {
                if let Some(mut arguments) = self.pending_arguments.remove(&item_key) {
                    arguments.lifecycle = lifecycle;
                    self.pending_arguments.insert(call_key, arguments);
                }
            }
        }
    }

    fn canonical_argument_key(
        &self,
        item_id: Option<&str>,
        call_id: Option<&str>,
    ) -> Option<FunctionArgumentKey> {
        call_id.map(FunctionArgumentKey::call).or_else(|| {
            item_id.map(|item_id| {
                self.item_to_call.get(item_id).map_or_else(
                    || FunctionArgumentKey::item(item_id),
                    |call_id| FunctionArgumentKey::call(call_id),
                )
            })
        })
    }

    fn ensure_argument_append(
        &self,
        current_length: usize,
        delta_length: usize,
    ) -> Result<(), LlmError> {
        let actual = current_length
            .checked_add(delta_length)
            .unwrap_or(usize::MAX);
        self.ensure_argument_bytes(actual)
    }

    fn ensure_argument_delta_allowed(
        &self,
        key: &FunctionArgumentKey,
        lifecycle: FunctionArgumentLifecycle,
    ) -> Result<(), LlmError> {
        if lifecycle == FunctionArgumentLifecycle::Finalized {
            return Err(LlmError::Message(format!(
                "Responses function argument {} received a delta after it was finalized",
                key.description()
            )));
        }
        Ok(())
    }

    fn ensure_argument_bytes(&self, actual: usize) -> Result<(), LlmError> {
        let actual = u64::try_from(actual).unwrap_or(u64::MAX);
        if actual > self.max_tool_call_argument_bytes {
            return Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                actual,
                maximum: self.max_tool_call_argument_bytes,
            });
        }
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

        self.remember_final_arguments(FunctionArgumentKey::call(call_id), arguments)?;
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
        &mut self,
        event: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<ResponsesTerminal, LlmError> {
        let response = response_object(event, "response.completed")?;
        let response_id = required_nonempty_string(response, "id", "response.completed response")?;
        self.reconcile_completed_output(response, events)?;
        self.validate_completed_items()?;
        let finish_reason = if self.emitted_tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCall
        };
        let usage = parse_usage(response)?;
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

    fn reconcile_completed_output(
        &mut self,
        response: &Value,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let Some(output) = response.get("output") else {
            return Ok(());
        };
        let output = output.as_array().ok_or_else(|| {
            LlmError::Message(
                "response.completed response contains a non-array field `output`".to_string(),
            )
        })?;
        let mut terminal_bindings = HashSet::new();
        for (output_index, item) in output.iter().enumerate() {
            let output_index = u64::try_from(output_index).map_err(|_| {
                LlmError::Message(
                    "response.completed output index exceeds the supported range".to_string(),
                )
            })?;
            let item_type =
                required_string(item, "type", "response.completed response output item")?;
            let (item_id, kind) = match item_type {
                "message" => {
                    let item_id = required_nonempty_string(
                        item,
                        "id",
                        "response.completed message output item",
                    )?;
                    self.handle_message_done_at_index(item, output_index, events)?;
                    (item_id, ResponseOutputItemKind::Message)
                }
                "function_call" => {
                    let item_id = required_nonempty_string(
                        item,
                        "id",
                        "response.completed function_call output item",
                    )?;
                    self.handle_function_call_done(item, item_id, output_index, events)?;
                    (item_id, ResponseOutputItemKind::FunctionCall)
                }
                "reasoning" => {
                    let item_id = required_nonempty_string(
                        item,
                        "id",
                        "response.completed reasoning output item",
                    )?;
                    self.handle_reasoning_item_done_at_index(item_id, output_index)?;
                    (item_id, ResponseOutputItemKind::Reasoning)
                }
                unsupported => {
                    return Err(LlmError::Message(format!(
                        "response.completed contains unsupported output item type `{unsupported}`"
                    )));
                }
            };
            terminal_bindings.insert((item_id.to_string(), output_index, kind));
        }

        if let Some((item_id, (output_index, kind))) = self
            .output_identities
            .item_to_output
            .iter()
            .find(|(item_id, (output_index, kind))| {
                !terminal_bindings.contains(&((*item_id).clone(), *output_index, *kind))
            })
        {
            return Err(LlmError::Message(format!(
                "response.completed output omitted observed {} item `{item_id}` at index `{output_index}`",
                kind.as_str()
            )));
        }
        Ok(())
    }

    fn validate_completed_items(&self) -> Result<(), LlmError> {
        if let Some((item_id, _)) = self
            .message_text_items
            .iter()
            .find(|(_, state)| !state.is_complete())
        {
            return Err(LlmError::Message(format!(
                "Responses stream completed before message item `{item_id}` was complete"
            )));
        }
        if let Some((item_id, _)) = self
            .function_items
            .iter()
            .find(|(_, state)| state.lifecycle != FunctionItemLifecycle::Complete)
        {
            return Err(LlmError::Message(format!(
                "Responses stream completed before function call item `{item_id}` was complete"
            )));
        }
        if let Some(item_id) = self.incomplete_reasoning_items.iter().next() {
            return Err(LlmError::Message(format!(
                "Responses stream completed before reasoning item `{item_id}` was complete"
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
        if let Some(key) = self.pending_arguments.keys().find(|key| match key {
            FunctionArgumentKey::Call(call_id) => !self.emitted_tool_calls.contains_key(call_id),
            FunctionArgumentKey::Item(item_id) => !self
                .item_to_call
                .get(item_id)
                .is_some_and(|call_id| self.emitted_tool_calls.contains_key(call_id)),
        }) {
            return Err(LlmError::Message(format!(
                "Responses stream completed with unresolved function arguments for {}",
                key.description()
            )));
        }
        Ok(())
    }

    fn apply_atomically<T, F>(
        &mut self,
        events: &mut Vec<LlmEvent>,
        update: F,
    ) -> Result<T, LlmError>
    where
        F: FnOnce(&mut Self, &mut Vec<LlmEvent>) -> Result<T, LlmError>,
    {
        let snapshot = self.clone();
        let event_count = events.len();
        match update(self, events) {
            Ok(value) => Ok(value),
            Err(error) => {
                *self = snapshot;
                events.truncate(event_count);
                Err(error)
            }
        }
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
        let usage = parse_usage(response)?;
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

fn required_u64(value: &Value, field: &str, context: &str) -> Result<u64, LlmError> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        LlmError::Message(format!(
            "{context} is missing unsigned integer field `{field}`"
        ))
    })
}

fn validate_assistant_message_role(item: &Value, context: &str) -> Result<(), LlmError> {
    let role = required_string(item, "role", context)?;
    if role != "assistant" {
        return Err(LlmError::Message(format!(
            "{context} contains unsupported message role `{role}`"
        )));
    }
    Ok(())
}

fn required_message_output_text(item: &Value, context: &str) -> Result<String, LlmError> {
    let content = item
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Message(format!("{context} is missing array field `content`")))?;
    collect_message_output_text(content, context)
}

fn validate_optional_message_output_text(item: &Value, context: &str) -> Result<(), LlmError> {
    let Some(content) = item.get("content") else {
        return Ok(());
    };
    let content = content.as_array().ok_or_else(|| {
        LlmError::Message(format!("{context} contains a non-array field `content`"))
    })?;
    collect_message_output_text(content, context).map(drop)
}

fn collect_message_output_text(content: &[Value], context: &str) -> Result<String, LlmError> {
    let mut text = String::new();
    for part in content {
        let part_type = required_string(part, "type", context)?;
        if part_type != "output_text" {
            return Err(LlmError::Message(format!(
                "{context} contains unsupported content part type `{part_type}`"
            )));
        }
        text.push_str(required_string(part, "text", context)?);
    }
    Ok(text)
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

fn parse_usage(response: &Value) -> Result<Option<TokenUsage>, LlmError> {
    let Some(usage) = response.get("usage") else {
        return Ok(None);
    };
    let usage = usage.as_object().ok_or_else(|| {
        LlmError::Message("Responses usage must be an object when present".to_string())
    })?;
    let prompt_tokens = required_usage_u32(usage, "input_tokens")?;
    let completion_tokens = required_usage_u32(usage, "output_tokens")?;
    let total_tokens = required_usage_u32(usage, "total_tokens")?;
    let reasoning_tokens = match usage.get("output_tokens_details") {
        None => None,
        Some(details) => {
            let details = details.as_object().ok_or_else(|| {
                LlmError::Message(
                    "Responses usage.output_tokens_details must be an object when present"
                        .to_string(),
                )
            })?;
            details
                .get("reasoning_tokens")
                .map(|value| {
                    let value = value.as_u64().ok_or_else(|| {
                        LlmError::Message(
                            "Responses usage.output_tokens_details.reasoning_tokens must be an unsigned integer"
                                .to_string(),
                        )
                    })?;
                    checked_usage_u32(value, "output_tokens_details.reasoning_tokens")
                })
                .transpose()?
        }
    };
    Ok(Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_tokens,
    }))
}

fn required_usage_u32(usage: &Map<String, Value>, field: &str) -> Result<u32, LlmError> {
    let value = usage.get(field).and_then(Value::as_u64).ok_or_else(|| {
        LlmError::Message(format!(
            "Responses usage.{field} must be an unsigned integer"
        ))
    })?;
    checked_usage_u32(value, field)
}

fn checked_usage_u32(value: u64, field: &str) -> Result<u32, LlmError> {
    u32::try_from(value).map_err(|_| {
        LlmError::Message(format!(
            "Responses usage.{field} exceeds the supported u32 range"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use super::*;
    use crate::config::model::{ProviderApiMode, ReasoningEffort};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::error::ProviderStreamLimit;
    use crate::llm::contract::{ModelCapabilities, ModelProfile, ModelToolCall, ToolSchema};

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
        }
    }

    #[test]
    fn request_replays_complete_non_system_history_and_standard_wire_shapes() {
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
            },
        )
        .expect("request should serialize");

        assert_eq!(
            wire["instructions"],
            json!("Base instructions\n\nAdditional policy")
        );
        assert!(wire.get("previous_response_id").is_none());
        assert_eq!(wire["store"], json!(false));
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
        assert_eq!(input.len(), 4);
        assert_eq!(input[0]["type"], json!("message"));
        assert_eq!(input[0]["role"], json!("user"));
        assert_eq!(input[0]["content"][0]["text"], json!("Already represented"));
        assert_eq!(input[1]["type"], json!("message"));
        assert_eq!(input[1]["role"], json!("assistant"));
        assert_eq!(input[2]["type"], json!("function_call"));
        assert_eq!(input[2]["call_id"], json!("call_1"));
        assert_eq!(input[3]["type"], json!("function_call_output"));
        assert_eq!(input[3]["output"], json!("contents"));
    }

    #[test]
    fn compacted_history_keeps_the_real_user_anchor_before_the_latest_summary() {
        let user_task = "Continue the calculator task.";
        let compacted_context = format!(
            "{}\nThe tests were inspected and the implementation remains pending.",
            include_str!("../../assets/prompts/compaction_summary_prefix.md").trim()
        );
        let request = request(vec![
            ModelMessage::Developer {
                content: "<multi_agent_mode>proactive</multi_agent_mode>".to_string(),
            },
            ModelMessage::Developer {
                content: "<sub_agent>Return verified evidence.</sub_agent>".to_string(),
            },
            ModelMessage::Agent {
                content: "Message Type: NEW_TASK\nPayload:\nInspect the calculator.".to_string(),
            },
            ModelMessage::User {
                content: user_task.to_string(),
            },
            ModelMessage::User {
                content: compacted_context.clone(),
            },
        ]);

        let wire = to_responses_request(
            &request,
            ResponsesRequestOptions {
                reasoning_capability: responses_capability(),
                ..ResponsesRequestOptions::default()
            },
        )
        .expect("cursor-less compacted Responses request");

        assert_eq!(
            wire["instructions"],
            json!(
                "Base instructions\n\n<multi_agent_mode>proactive</multi_agent_mode>\n\n<sub_agent>Return verified evidence.</sub_agent>"
            )
        );
        assert!(wire.get("previous_response_id").is_none());
        assert_eq!(
            wire["input"],
            json!([{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Message Type: NEW_TASK\nPayload:\nInspect the calculator." }]
            }, {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": user_task }]
            }, {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": compacted_context }]
            }])
        );
        assert!(
            !wire["input"]
                .as_array()
                .expect("input array")
                .iter()
                .any(|item| item["role"] == json!("developer"))
        );
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
            },
        )
        .expect("request should preserve explicit provider settings");

        assert_eq!(wire["model"], json!("gpt-test"));
        assert_eq!(wire["input"][0]["role"], json!("user"));
        assert!(wire.get("previous_response_id").is_none());
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
    fn request_rejects_invalid_reasoning_capability() {
        let request = request(vec![ModelMessage::System {
            content: "Only system".to_string(),
        }]);
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

        let options = ResponsesRequestOptions::from_request(&request);
        assert_eq!(options.reasoning_request, request.reasoning.as_ref());
        assert_eq!(options.reasoning_capability, request.reasoning_capability);
    }

    #[test]
    fn output_text_and_reasoning_summary_are_typed_without_raw_reasoning() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let text = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
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
                "output_index": 0,
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
                "output_index": 0,
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
    fn message_done_emits_only_the_missing_suffix_and_rejects_conflicting_text() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let first_delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_suffix",
                "output_index": 0,
                "delta": "hel"
            }))
            .expect("prefix delta");
        assert!(matches!(
            first_delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "hel"
        ));

        let done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_suffix",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hello" }]
                }
            }))
            .expect("message completion");
        assert!(matches!(
            done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "lo"
        ));

        let duplicate = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_suffix",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hello" }]
                }
            }))
            .expect("exact duplicate completion");
        assert!(duplicate.events.is_empty());

        let before = response_text_state_snapshot(&accumulator);
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_suffix",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "help" }]
                }
            }))
            .expect_err("conflicting duplicate completion must fail closed");
        assert_eq!(response_text_state_snapshot(&accumulator), before);
    }

    #[test]
    fn empty_text_delta_does_not_suppress_completed_text() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let empty_delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_empty_delta",
                "output_index": 0,
                "delta": ""
            }))
            .expect("empty delta is a typed stream event");
        assert!(matches!(
            empty_delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text.is_empty()
        ));

        let done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_empty_delta",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "complete text" }]
                }
            }))
            .expect("done must supply text missing after an empty delta");
        assert!(matches!(
            done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "complete text"
        ));
    }

    #[test]
    fn output_text_done_and_completed_output_reconcile_without_duplicate_projection() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_lifecycle",
                    "role": "assistant",
                    "content": []
                }
            }))
            .expect("message added");
        let delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_lifecycle",
                "output_index": 0,
                "delta": "hel"
            }))
            .expect("message delta");
        assert!(matches!(
            delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "hel"
        ));
        let text_done = accumulator
            .push_value(&json!({
                "type": "response.output_text.done",
                "item_id": "msg_lifecycle",
                "output_index": 0,
                "text": "hello"
            }))
            .expect("text done");
        assert!(matches!(
            text_done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "lo"
        ));
        let item_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_lifecycle",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hello" }]
                }
            }))
            .expect("item done");
        assert!(item_done.events.is_empty());

        let completed = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_lifecycle",
                    "output": [{
                        "type": "message",
                        "id": "msg_lifecycle",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "hello" }]
                    }]
                }
            }))
            .expect("terminal reconciliation");
        assert!(matches!(
            completed.events.as_slice(),
            [LlmEvent::Finished {
                finish_reason: FinishReason::Stop,
                usage: None,
            }]
        ));
    }

    #[test]
    fn completed_output_projects_terminal_only_supported_items_once() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let completed = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_terminal_only",
                    "output": [
                        { "type": "reasoning", "id": "rs_terminal" },
                        {
                            "type": "message",
                            "id": "msg_terminal",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": "ready" }]
                        },
                        {
                            "type": "function_call",
                            "id": "fc_terminal",
                            "call_id": "call_terminal",
                            "name": "read_file",
                            "arguments": "{}"
                        }
                    ]
                }
            }))
            .expect("terminal-only output");

        assert!(matches!(
            completed.events.as_slice(),
            [
                LlmEvent::TextDelta(text),
                LlmEvent::ToolCallStart { call_id, tool_name },
                LlmEvent::ToolCallArgsDelta { call_id: args_id, delta },
                LlmEvent::Finished {
                    finish_reason: FinishReason::ToolCall,
                    usage: None,
                },
            ] if text == "ready"
                && call_id == "call_terminal"
                && tool_name == "read_file"
                && args_id == "call_terminal"
                && delta == "{}"
        ));
    }

    #[test]
    fn completed_output_conflicts_and_omissions_roll_back_all_reconciliation_state() {
        let mut conflicting = ResponsesStreamAccumulator::default();
        conflicting
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_observed",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "observed" }]
                }
            }))
            .expect("observed message");
        let before = response_text_state_snapshot(&conflicting);
        conflicting
            .push_value(&json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_conflict",
                    "output": [{
                        "type": "message",
                        "id": "msg_observed",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "changed" }]
                    }]
                }
            }))
            .expect_err("terminal text conflict");
        assert_eq!(response_text_state_snapshot(&conflicting), before);

        let mut omitted = ResponsesStreamAccumulator::default();
        omitted
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_omitted",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "visible" }]
                }
            }))
            .expect("observed message");
        let before = response_text_state_snapshot(&omitted);
        let error = omitted
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_omitted", "output": [] }
            }))
            .expect_err("terminal output omission");
        assert!(error.to_string().contains("omitted"));
        assert_eq!(response_text_state_snapshot(&omitted), before);
    }

    #[test]
    fn output_text_done_still_requires_item_completion() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let done = accumulator
            .push_value(&json!({
                "type": "response.output_text.done",
                "item_id": "msg_text_done_only",
                "output_index": 0,
                "text": "complete text"
            }))
            .expect("text done");
        assert!(matches!(
            done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "complete text"
        ));
        let before = response_text_state_snapshot(&accumulator);
        accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_without_item_done" }
            }))
            .expect_err("text done cannot replace item done");
        assert_eq!(response_text_state_snapshot(&accumulator), before);
    }

    #[test]
    fn message_done_rejects_text_that_does_not_extend_streamed_deltas() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_conflicting_prefix",
                "output_index": 0,
                "delta": "streamed"
            }))
            .expect("streamed prefix");
        let before = response_text_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_conflicting_prefix",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "different" }]
                }
            }))
            .expect_err("done text must extend the emitted delta prefix");

        assert!(error.to_string().contains("conflicts"));
        assert_eq!(response_text_state_snapshot(&accumulator), before);
    }

    #[test]
    fn every_output_text_part_is_validated_before_text_state_mutates() {
        let malformed_done = json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_parts",
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "valid prefix" },
                    { "type": "output_text", "text": 7 }
                ]
            }
        });
        let malformed_added = json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_parts",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": null }]
            }
        });

        for malformed in [malformed_added, malformed_done] {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = response_text_state_snapshot(&accumulator);
            accumulator
                .push_value(&malformed)
                .expect_err("malformed output_text must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);

            let valid = accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_parts",
                        "role": "assistant",
                        "content": [
                            { "type": "output_text", "text": "valid" },
                            { "type": "output_text", "text": " text" }
                        ]
                    }
                }))
                .expect("rejected event must leave the item reusable");
            assert!(matches!(
                valid.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "valid text"
            ));
        }
    }

    #[test]
    fn output_items_require_explicit_supported_types_without_mutation() {
        let invalid_events = [
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": "missing_added_type",
                    "role": "assistant",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "id": "missing_done_type",
                    "role": "assistant",
                    "content": []
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_missing_type",
                    "output": [{
                        "id": "missing_terminal_type",
                        "role": "assistant",
                        "content": []
                    }]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "computer_call", "id": "unsupported_added" }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": { "type": "computer_call", "id": "unsupported_done" }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_unsupported_type",
                    "output": [
                        {
                            "type": "function_call",
                            "id": "fc_before_unsupported",
                            "call_id": "call_before_unsupported",
                            "name": "read_file",
                            "arguments": "{}"
                        },
                        { "type": "computer_call", "id": "unsupported_terminal" }
                    ]
                }
            }),
        ];

        for event in invalid_events {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = accumulator_state_snapshot(&accumulator);
            let error = accumulator
                .push_value(&event)
                .expect_err("missing and unsupported output types must fail closed");
            assert!(error.to_string().contains("type"));
            assert_eq!(accumulator_state_snapshot(&accumulator), before);
        }
    }

    #[test]
    fn message_outputs_require_assistant_role_and_supported_content_without_mutation() {
        let invalid_events = [
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_added_user",
                    "role": "user",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_done_user",
                    "role": "user",
                    "content": []
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_user_role",
                    "output": [{
                        "type": "message",
                        "id": "msg_terminal_user",
                        "role": "user",
                        "content": []
                    }]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_added_refusal",
                    "role": "assistant",
                    "content": [{ "type": "refusal", "refusal": "no" }]
                }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_done_refusal",
                    "role": "assistant",
                    "content": [{ "type": "refusal", "refusal": "no" }]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_unsupported_content",
                    "output": [{
                        "type": "message",
                        "id": "msg_terminal_audio",
                        "role": "assistant",
                        "content": [{ "type": "output_audio", "audio": "bytes" }]
                    }]
                }
            }),
        ];

        for event in invalid_events {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = accumulator_state_snapshot(&accumulator);
            let error = accumulator
                .push_value(&event)
                .expect_err("unsupported roles and content must fail closed");
            assert!(error.to_string().contains("unsupported"));
            assert_eq!(accumulator_state_snapshot(&accumulator), before);
        }
    }

    #[test]
    fn added_or_streaming_message_requires_done_before_response_completed() {
        for partial in [
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_incomplete",
                    "role": "assistant",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_incomplete",
                "output_index": 0,
                "delta": "partial"
            }),
        ] {
            let mut accumulator = ResponsesStreamAccumulator::default();
            accumulator
                .push_value(&partial)
                .expect("partial message event");
            let before = response_text_state_snapshot(&accumulator);
            let error = accumulator
                .push_value(&json!({
                    "type": "response.completed",
                    "response": { "id": "resp_too_early" }
                }))
                .expect_err("response completion requires message done");
            assert!(error.to_string().contains("msg_incomplete"));
            assert_eq!(response_text_state_snapshot(&accumulator), before);
        }
    }

    #[test]
    fn text_delta_and_message_added_after_done_are_rejected_without_mutation() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_done",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "done" }]
                }
            }))
            .expect("completed message");
        let before = response_text_state_snapshot(&accumulator);

        for late in [
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_done",
                "output_index": 0,
                "delta": "late"
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_done",
                    "role": "assistant",
                    "content": []
                }
            }),
        ] {
            accumulator
                .push_value(&late)
                .expect_err("events after message done must fail closed");
            assert_eq!(response_text_state_snapshot(&accumulator), before);
        }
    }

    #[test]
    fn text_delta_deduplicates_only_its_bound_message_item() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
                "delta": "first"
            }))
            .expect("first message delta");
        assert!(matches!(
            delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "first"
        ));

        let first_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "first" }]
                }
            }))
            .expect("first message done");
        assert!(first_done.events.is_empty());

        let second_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "second" }]
                }
            }))
            .expect("second message done");
        assert!(matches!(
            second_done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "second"
        ));
    }

    #[test]
    fn text_delta_for_another_bound_message_is_not_dropped() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let first_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "first" }]
                }
            }))
            .expect("first message done");
        assert!(matches!(
            first_done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "first"
        ));

        let second_delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_2",
                "output_index": 1,
                "delta": "second"
            }))
            .expect("second message delta");
        assert!(matches!(
            second_delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "second"
        ));

        let second_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "second" }]
                }
            }))
            .expect("second message done");
        assert!(second_done.events.is_empty());
    }

    #[test]
    fn text_events_reject_missing_blank_or_malformed_identity_without_mutation() {
        let invalid_deltas = [
            json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "invalid"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": " \t",
                "output_index": 0,
                "delta": "invalid"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": 7,
                "output_index": 0,
                "delta": "invalid"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "delta": "invalid"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": "0",
                "delta": "invalid"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": -1,
                "delta": "invalid"
            }),
        ];
        for invalid in invalid_deltas {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = response_text_state_snapshot(&accumulator);
            accumulator
                .push_value(&invalid)
                .expect_err("invalid text delta identity must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);
            let valid_done = accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "complete" }]
                    }
                }))
                .expect("invalid delta must not retain text state");
            assert!(matches!(
                valid_done.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "complete"
            ));
        }

        let invalid_done_items = [
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "invalid" }]
            }),
            json!({
                "type": "message",
                "id": " \t",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "invalid" }]
            }),
            json!({
                "type": "message",
                "id": 7,
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "invalid" }]
            }),
        ];
        for invalid_item in invalid_done_items {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = response_text_state_snapshot(&accumulator);
            accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": invalid_item
                }))
                .expect_err("invalid message item id must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);
            let valid_done = accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "complete" }]
                    }
                }))
                .expect("invalid done event must not retain text state");
            assert!(matches!(
                valid_done.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "complete"
            ));
        }

        for invalid_index in [Value::Null, json!("0"), json!(-1)] {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = response_text_state_snapshot(&accumulator);
            let mut event = json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "invalid" }]
                }
            });
            if !invalid_index.is_null() {
                event["output_index"] = invalid_index;
            }
            accumulator
                .push_value(&event)
                .expect_err("invalid message output_index must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);
            let valid_done = accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "complete" }]
                    }
                }))
                .expect("invalid done index must not retain text state");
            assert!(matches!(
                valid_done.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "complete"
            ));
        }
    }

    #[test]
    fn message_added_requires_the_same_typed_identity_without_mutation() {
        for invalid in [
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "message", "role": "assistant", "content": [] }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "message", "id": " ", "role": "assistant", "content": [] }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": "0",
                "item": { "type": "message", "id": "msg_1", "role": "assistant", "content": [] }
            }),
        ] {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = response_text_state_snapshot(&accumulator);
            accumulator
                .push_value(&invalid)
                .expect_err("invalid message added identity must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);
            let valid_done = accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "complete" }]
                    }
                }))
                .expect("invalid added event must not retain text state");
            assert!(matches!(
                valid_done.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "complete"
            ));
        }
    }

    #[test]
    fn duplicate_identityless_message_done_cannot_reemit_text() {
        let invalid_done = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "must not emit" }]
            }
        });
        let mut accumulator = ResponsesStreamAccumulator::default();
        let before = response_text_state_snapshot(&accumulator);

        accumulator
            .push_value(&invalid_done)
            .expect_err("identity-less done must be rejected");
        assert_eq!(response_text_state_snapshot(&accumulator), before);
        accumulator
            .push_value(&invalid_done)
            .expect_err("duplicate identity-less done must remain rejected");
        assert_eq!(response_text_state_snapshot(&accumulator), before);

        let valid_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "valid" }]
                }
            }))
            .expect("rejected events must not retain completion state");
        assert!(matches!(
            valid_done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "valid"
        ));
    }

    #[test]
    fn conflicting_text_identity_aliases_are_rejected_without_mutation() {
        for conflicting in [
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 1,
                "delta": "conflict"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_2",
                "output_index": 0,
                "delta": "conflict"
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "conflict" }]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": []
                }
            }),
        ] {
            let mut accumulator = ResponsesStreamAccumulator::default();
            accumulator
                .push_value(&json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": []
                    }
                }))
                .expect("initial identity binding");
            let before = response_text_state_snapshot(&accumulator);
            accumulator
                .push_value(&conflicting)
                .expect_err("conflicting text identity aliases must be rejected");
            assert_eq!(response_text_state_snapshot(&accumulator), before);

            let valid_delta = accumulator
                .push_value(&json!({
                    "type": "response.output_text.delta",
                    "item_id": "msg_1",
                    "output_index": 0,
                    "delta": "valid"
                }))
                .expect("conflict must not damage the original binding");
            assert!(matches!(
                valid_delta.events.as_slice(),
                [LlmEvent::TextDelta(text)] if text == "valid"
            ));
        }
    }

    #[test]
    fn distinct_text_identities_emit_and_deduplicate_independently() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        for (item_id, output_index) in [("msg_1", 0), ("msg_2", 1)] {
            accumulator
                .push_value(&json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "type": "message",
                        "id": item_id,
                        "role": "assistant",
                        "content": []
                    }
                }))
                .expect("message identity added");
        }

        let first_delta = accumulator
            .push_value(&json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
                "delta": "first"
            }))
            .expect("first delta");
        assert!(matches!(
            first_delta.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "first"
        ));
        let first_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "first" }]
                }
            }))
            .expect("first done");
        assert!(first_done.events.is_empty());

        let second_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "second" }]
                }
            }))
            .expect("second done");
        assert!(matches!(
            second_done.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "second"
        ));
        let duplicate_second_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "second" }]
                }
            }))
            .expect("duplicate second done");
        assert!(duplicate_second_done.events.is_empty());
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
                "output_index": 0,
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
                "output_index": 0,
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
                    "output": [{
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "read_file",
                        "arguments": "{\"path\":\"README.md\"}"
                    }],
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
    fn function_arguments_done_does_not_complete_a_tool_without_output_item_done() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let arguments_done = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_arguments_only",
                "output_index": 0,
                "call_id": "call_arguments_only",
                "name": "read_file",
                "arguments": "{}"
            }))
            .expect("final arguments remain pending");
        assert!(arguments_done.events.is_empty());
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_arguments_only" }
            }))
            .expect_err("output_item.done remains required");
        assert!(error.to_string().contains("complete"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn function_added_identity_is_exact_cross_kind_and_rejects_late_added() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_exact",
                    "call_id": "call_exact",
                    "name": "read_file",
                    "arguments": ""
                }
            }))
            .expect("function added");
        let before_conflict = function_call_state_snapshot(&accumulator);
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_exact",
                    "call_id": "call_changed",
                    "name": "write_file",
                    "arguments": "{}"
                }
            }))
            .expect_err("function identity cannot change");
        assert_eq!(function_call_state_snapshot(&accumulator), before_conflict);

        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_exact",
                    "call_id": "call_exact",
                    "name": "read_file",
                    "arguments": "{}"
                }
            }))
            .expect("exact function completion");
        let complete = function_call_state_snapshot(&accumulator);

        accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_exact",
                    "call_id": "call_exact",
                    "name": "read_file",
                    "arguments": ""
                }
            }))
            .expect_err("late added is invalid");
        assert_eq!(function_call_state_snapshot(&accumulator), complete);

        let message_error = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_alias",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "invalid alias" }]
                }
            }))
            .expect_err("output index cannot alias across item kinds");
        assert!(message_error.to_string().contains("output index"));
        assert_eq!(function_call_state_snapshot(&accumulator), complete);
    }

    fn assert_no_function_call_state(accumulator: &ResponsesStreamAccumulator) {
        assert!(accumulator.pending_arguments.is_empty());
        assert!(accumulator.item_to_call.is_empty());
        assert!(accumulator.call_to_item.is_empty());
        assert!(accumulator.function_items.is_empty());
        assert!(accumulator.emitted_tool_calls.is_empty());
        assert!(accumulator.terminal().is_none());
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ResponseTextStateSnapshot {
        identities: ResponseOutputIdentityRegistry,
        items: HashMap<String, MessageTextState>,
        terminal_present: bool,
    }

    fn response_text_state_snapshot(
        accumulator: &ResponsesStreamAccumulator,
    ) -> ResponseTextStateSnapshot {
        ResponseTextStateSnapshot {
            identities: accumulator.output_identities.clone(),
            items: accumulator.message_text_items.clone(),
            terminal_present: accumulator.terminal().is_some(),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AccumulatorStateSnapshot {
        pending_arguments: HashMap<FunctionArgumentKey, FunctionArgumentState>,
        item_to_call: HashMap<String, String>,
        call_to_item: HashMap<String, String>,
        function_items: HashMap<String, FunctionItemState>,
        incomplete_reasoning_items: HashSet<String>,
        completed_reasoning_items: HashSet<String>,
        emitted_tool_calls: BTreeMap<String, (String, String)>,
        output_identities: ResponseOutputIdentityRegistry,
        message_text_items: HashMap<String, MessageTextState>,
        terminal_present: bool,
    }

    fn accumulator_state_snapshot(
        accumulator: &ResponsesStreamAccumulator,
    ) -> AccumulatorStateSnapshot {
        AccumulatorStateSnapshot {
            pending_arguments: accumulator.pending_arguments.clone(),
            item_to_call: accumulator.item_to_call.clone(),
            call_to_item: accumulator.call_to_item.clone(),
            function_items: accumulator.function_items.clone(),
            incomplete_reasoning_items: accumulator.incomplete_reasoning_items.clone(),
            completed_reasoning_items: accumulator.completed_reasoning_items.clone(),
            emitted_tool_calls: accumulator
                .emitted_tool_calls
                .iter()
                .map(|(call_id, call)| {
                    (call_id.clone(), (call.name.clone(), call.arguments.clone()))
                })
                .collect(),
            output_identities: accumulator.output_identities.clone(),
            message_text_items: accumulator.message_text_items.clone(),
            terminal_present: accumulator.terminal().is_some(),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FunctionCallStateSnapshot {
        output_identities: ResponseOutputIdentityRegistry,
        pending_arguments: HashMap<FunctionArgumentKey, FunctionArgumentState>,
        item_to_call: HashMap<String, String>,
        call_to_item: HashMap<String, String>,
        function_items: HashMap<String, FunctionItemState>,
        emitted_tool_calls: BTreeMap<String, (String, String)>,
        terminal_present: bool,
    }

    fn function_call_state_snapshot(
        accumulator: &ResponsesStreamAccumulator,
    ) -> FunctionCallStateSnapshot {
        FunctionCallStateSnapshot {
            output_identities: accumulator.output_identities.clone(),
            pending_arguments: accumulator.pending_arguments.clone(),
            item_to_call: accumulator.item_to_call.clone(),
            call_to_item: accumulator.call_to_item.clone(),
            function_items: accumulator.function_items.clone(),
            emitted_tool_calls: accumulator
                .emitted_tool_calls
                .iter()
                .map(|(call_id, call)| {
                    (call_id.clone(), (call.name.clone(), call.arguments.clone()))
                })
                .collect(),
            terminal_present: accumulator.terminal().is_some(),
        }
    }

    fn preseeded_function_call_accumulator() -> ResponsesStreamAccumulator {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_pending",
                "delta": "{"
            }))
            .expect("pending argument seed");
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_emitted",
                    "call_id": "call_emitted",
                    "name": "read_file",
                    "arguments": "{}"
                }
            }))
            .expect("emitted call seed");
        accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "item": {
                    "type": "function_call",
                    "id": "fc_incomplete",
                    "call_id": "call_incomplete",
                    "name": "read_file",
                    "arguments": ""
                }
            }))
            .expect("incomplete item seed");
        accumulator
    }

    #[test]
    fn output_item_done_rejects_blank_item_id_without_mutation() {
        let mut accumulator = preseeded_function_call_accumulator();
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": " \t",
                    "call_id": "call_invalid",
                    "name": "read_file",
                    "arguments": "{}"
                }
            }))
            .expect_err("blank function item id must be rejected");

        assert!(error.to_string().contains("id"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn output_item_added_rejects_blank_call_id_without_mutation() {
        let mut accumulator = preseeded_function_call_accumulator();
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": "fc_invalid",
                    "call_id": " \t",
                    "name": "read_file"
                }
            }))
            .expect_err("blank function call id must be rejected");

        assert!(error.to_string().contains("call_id"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn output_item_added_rejects_blank_name_without_mutation() {
        let mut accumulator = preseeded_function_call_accumulator();
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.added",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": "fc_invalid",
                    "call_id": "call_invalid",
                    "name": " \t"
                }
            }))
            .expect_err("blank function name must be rejected");

        assert!(error.to_string().contains("name"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn function_argument_delta_rejects_blank_identity_fields_without_mutation() {
        for (blank_field, event) in [
            (
                "item_id",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": " \t",
                    "call_id": "call_valid",
                    "delta": "{}"
                }),
            ),
            (
                "call_id",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": "item_valid",
                    "call_id": " \t",
                    "delta": "{}"
                }),
            ),
        ] {
            let mut accumulator = ResponsesStreamAccumulator::default();

            let error = accumulator
                .push_value(&event)
                .expect_err("blank identity must be rejected");

            assert!(error.to_string().contains(blank_field));
            assert_no_function_call_state(&accumulator);
        }
    }

    #[test]
    fn function_arguments_done_rejects_blank_identity_fields_without_mutation() {
        for (blank_field, event) in [
            (
                "item_id",
                json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": " \t",
                    "call_id": "call_valid",
                    "name": "read_file",
                    "arguments": "{}"
                }),
            ),
            (
                "call_id",
                json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": "item_valid",
                    "call_id": " \t",
                    "name": "read_file",
                    "arguments": "{}"
                }),
            ),
        ] {
            let mut accumulator = ResponsesStreamAccumulator::default();

            let error = accumulator
                .push_value(&event)
                .expect_err("blank identity must be rejected");

            assert!(error.to_string().contains(blank_field));
            assert_no_function_call_state(&accumulator);
        }
    }

    #[test]
    fn function_arguments_done_rejects_blank_name_without_mutation() {
        let mut accumulator = ResponsesStreamAccumulator::default();

        let error = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "call_id": "call_valid",
                "name": " \t",
                "arguments": "{}"
            }))
            .expect_err("blank function name must be rejected");

        assert!(error.to_string().contains("name"));
        assert_no_function_call_state(&accumulator);
    }

    #[test]
    fn function_item_done_rolls_back_new_aliases_when_arguments_conflict() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "call_id": "call_conflict",
                "delta": "{\"path\":\"README.md\"}"
            }))
            .expect("call-keyed arguments");
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_new_alias",
                    "call_id": "call_conflict",
                    "name": "read_file",
                    "arguments": "{\"path\":\"other.md\"}"
                }
            }))
            .expect_err("conflicting inline arguments must reject the new alias");

        assert!(error.to_string().contains("conflicting function arguments"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn function_item_done_rejects_non_string_arguments_without_mutation() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "call_id": "call_typed_arguments",
                "delta": "{}"
            }))
            .expect("call-keyed arguments");
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_typed_arguments",
                    "call_id": "call_typed_arguments",
                    "name": "read_file",
                    "arguments": { "path": "README.md" }
                }
            }))
            .expect_err("inline arguments must remain a raw string");

        assert!(error.to_string().contains("arguments"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn function_arguments_done_rolls_back_alias_and_argument_conflicts() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_pending_alias",
                "delta": "{\"path\":"
            }))
            .expect("item-keyed arguments");
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_pending_alias",
                "call_id": "call_new_alias",
                "name": "read_file",
                "arguments": "[]"
            }))
            .expect_err("conflicting final arguments must roll back alias binding");

        assert!(error.to_string().contains("conflicting arguments"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn function_arguments_done_rejects_conflicting_emitted_name_without_mutation() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_emitted_name",
                    "call_id": "call_emitted_name",
                    "name": "read_file",
                    "arguments": "{}"
                }
            }))
            .expect("emitted function call");
        let before = function_call_state_snapshot(&accumulator);

        let error = accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_emitted_name",
                "call_id": "call_emitted_name",
                "name": "write_file",
                "arguments": "{}"
            }))
            .expect_err("an emitted call name cannot change");

        assert!(error.to_string().contains("name"));
        assert_eq!(function_call_state_snapshot(&accumulator), before);
    }

    #[test]
    fn oversized_function_argument_delta_is_rejected_before_retention() {
        let maximum = 4;
        let mut accumulator = ResponsesStreamAccumulator::new(maximum);
        let result = accumulator.push_value(&json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_oversized_delta",
            "delta": "12345"
        }));

        assert!(
            accumulator.pending_arguments.is_empty(),
            "an oversized delta must not mutate retained argument state"
        );
        assert!(matches!(
            result,
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                actual,
                maximum: admitted_maximum,
            }) if actual == maximum + 1 && admitted_maximum == maximum
        ));
    }

    #[test]
    fn oversized_output_item_arguments_are_rejected_before_emission() {
        let maximum = 4;
        let mut accumulator = ResponsesStreamAccumulator::new(maximum);
        let result = accumulator.push_value(&json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_oversized_item",
                "call_id": "call_oversized_item",
                "name": "read_file",
                "arguments": "12345"
            }
        }));

        assert!(accumulator.pending_arguments.is_empty());
        assert!(accumulator.item_to_call.is_empty());
        assert!(accumulator.call_to_item.is_empty());
        assert!(accumulator.emitted_tool_calls.is_empty());
        assert!(matches!(
            result,
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                actual,
                maximum: admitted_maximum,
            }) if actual == maximum + 1 && admitted_maximum == maximum
        ));
    }

    #[test]
    fn item_and_call_aliases_retain_one_canonical_argument_value() {
        let arguments = "{\"path\":\"README.md\"}";
        let mut accumulator = ResponsesStreamAccumulator::new(arguments.len() as u64);
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_canonical",
                "delta": arguments
            }))
            .expect("arguments delta");
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_canonical",
                "arguments": arguments
            }))
            .expect("arguments done");
        let update = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_canonical",
                    "call_id": "call_canonical",
                    "name": "read_file",
                    "arguments": arguments
                }
            }))
            .expect("function call item");

        assert_eq!(accumulator.pending_arguments.len(), 1);
        assert_eq!(
            accumulator
                .call_to_item
                .get("call_canonical")
                .map(String::as_str),
            Some("fc_canonical")
        );
        assert_eq!(
            accumulator
                .pending_arguments
                .get(&FunctionArgumentKey::call("call_canonical"))
                .map(FunctionArgumentState::as_str),
            Some(arguments)
        );
        assert!(
            accumulator
                .pending_arguments
                .get(&FunctionArgumentKey::call("call_canonical"))
                .is_some_and(FunctionArgumentState::is_finalized)
        );
        assert!(
            !accumulator
                .pending_arguments
                .contains_key(&FunctionArgumentKey::item("fc_canonical"))
        );
        assert!(matches!(
            update.events.as_slice(),
            [
                LlmEvent::ToolCallStart { call_id, .. },
                LlmEvent::ToolCallArgsDelta { call_id: args_id, delta }
            ] if call_id == "call_canonical"
                && args_id == "call_canonical"
                && delta == arguments
        ));
    }

    #[test]
    fn argument_delta_limit_rejects_before_extending_existing_value() {
        let mut accumulator = ResponsesStreamAccumulator::new(4);
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_boundary",
                "delta": "1234"
            }))
            .expect("argument boundary");

        let result = accumulator.push_value(&json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_boundary",
            "delta": "5"
        }));

        assert_eq!(
            accumulator
                .pending_arguments
                .get(&FunctionArgumentKey::item("fc_boundary"))
                .map(FunctionArgumentState::as_str),
            Some("1234")
        );
        assert!(matches!(
            result,
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                actual: 5,
                maximum: 4,
            })
        ));
    }

    #[test]
    fn equal_item_and_call_id_text_use_distinct_provisional_keys() {
        let mut accumulator = ResponsesStreamAccumulator::new(4);
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "shared_id",
                "delta": "12"
            }))
            .expect("item-keyed delta");
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "call_id": "shared_id",
                "delta": "34"
            }))
            .expect("call-keyed delta");

        assert_eq!(
            accumulator
                .pending_arguments
                .get(&FunctionArgumentKey::item("shared_id"))
                .map(FunctionArgumentState::as_str),
            Some("12")
        );
        assert_eq!(
            accumulator
                .pending_arguments
                .get(&FunctionArgumentKey::call("shared_id"))
                .map(FunctionArgumentState::as_str),
            Some("34")
        );
    }

    #[test]
    fn finalized_output_item_rejects_late_delta_without_mutation() {
        for identity_field in ["item_id", "call_id"] {
            let mut accumulator = ResponsesStreamAccumulator::new(64);
            accumulator
                .push_value(&json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "id": "fc_finalized",
                        "call_id": "call_finalized",
                        "name": "read_file",
                        "arguments": "{\"path\":\"README.md\"}"
                    }
                }))
                .expect("finalized output item");
            let pending_before = accumulator.pending_arguments.clone();
            let bindings_before = accumulator.item_to_call.clone();
            let reverse_bindings_before = accumulator.call_to_item.clone();
            let emitted_before = accumulator
                .emitted_tool_calls
                .get("call_finalized")
                .map(|call| (call.name.clone(), call.arguments.clone()));
            let mut late_delta = json!({
                "type": "response.function_call_arguments.delta",
                "delta": " "
            });
            late_delta[identity_field] = json!(if identity_field == "item_id" {
                "fc_finalized"
            } else {
                "call_finalized"
            });

            let result = accumulator.push_value(&late_delta);

            assert_eq!(accumulator.pending_arguments, pending_before);
            assert_eq!(accumulator.item_to_call, bindings_before);
            assert_eq!(accumulator.call_to_item, reverse_bindings_before);
            assert_eq!(
                accumulator
                    .emitted_tool_calls
                    .get("call_finalized")
                    .map(|call| (call.name.clone(), call.arguments.clone())),
                emitted_before
            );
            assert!(accumulator.terminal().is_none());
            assert!(matches!(
                result,
                Err(LlmError::Message(message)) if message.contains("finalized")
            ));
        }
    }

    #[test]
    fn finalized_arguments_done_rejects_late_delta_without_mutation() {
        let mut accumulator = ResponsesStreamAccumulator::new(64);
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.done",
                "item_id": "fc_arguments_done",
                "arguments": "{\"path\":\"README.md\"}"
            }))
            .expect("final arguments");
        let pending_before = accumulator.pending_arguments.clone();
        let bindings_before = accumulator.item_to_call.clone();
        let reverse_bindings_before = accumulator.call_to_item.clone();

        let result = accumulator.push_value(&json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_arguments_done",
            "delta": " "
        }));

        assert_eq!(accumulator.pending_arguments, pending_before);
        assert_eq!(accumulator.item_to_call, bindings_before);
        assert_eq!(accumulator.call_to_item, reverse_bindings_before);
        assert!(accumulator.emitted_tool_calls.is_empty());
        assert!(accumulator.terminal().is_none());
        assert!(matches!(
            result,
            Err(LlmError::Message(message)) if message.contains("finalized")
        ));
    }

    #[test]
    fn distinct_items_cannot_bind_to_the_same_call() {
        let arguments = "{\"path\":\"README.md\"}";
        let mut accumulator = ResponsesStreamAccumulator::new(64);
        accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_first",
                    "call_id": "call_shared",
                    "name": "read_file",
                    "arguments": arguments
                }
            }))
            .expect("first function call item");
        accumulator
            .push_value(&json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_second",
                "delta": arguments
            }))
            .expect("second provisional item");
        let pending_before = accumulator.pending_arguments.clone();
        let bindings_before = accumulator.item_to_call.clone();
        let reverse_bindings_before = accumulator.call_to_item.clone();
        let emitted_before = accumulator
            .emitted_tool_calls
            .get("call_shared")
            .map(|call| (call.name.clone(), call.arguments.clone()));

        let result = accumulator.push_value(&json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_second",
                "call_id": "call_shared",
                "name": "read_file",
                "arguments": arguments
            }
        }));

        assert_eq!(accumulator.pending_arguments, pending_before);
        assert_eq!(accumulator.item_to_call, bindings_before);
        assert_eq!(accumulator.call_to_item, reverse_bindings_before);
        assert_eq!(
            accumulator
                .emitted_tool_calls
                .get("call_shared")
                .map(|call| (call.name.clone(), call.arguments.clone())),
            emitted_before
        );
        assert!(matches!(
            result,
            Err(LlmError::Message(message)) if message.contains("multiple items")
        ));
    }

    #[test]
    fn malformed_function_arguments_are_preserved_for_runtime_parsing() {
        let mut accumulator = ResponsesStreamAccumulator::default();
        let item_done = accumulator
            .push_value(&json!({
                "type": "response.output_item.done",
                "output_index": 0,
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
                "response": {
                    "id": "resp_raw",
                    "output": [{
                        "type": "function_call",
                        "id": "fc_raw",
                        "call_id": "call_raw",
                        "name": "read_file",
                        "arguments": "{\"path\":"
                    }]
                }
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
                "output_index": 0,
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
    fn usage_is_optional_for_completed_and_incomplete_terminals() {
        let mut completed_accumulator = ResponsesStreamAccumulator::default();
        let completed = completed_accumulator
            .push_value(&json!({
                "type": "response.completed",
                "response": { "id": "resp_without_usage", "output": [] }
            }))
            .expect("completed usage may be absent");
        assert!(matches!(
            completed.terminal,
            Some(ResponsesTerminal::Completed { usage: None, .. })
        ));

        let mut incomplete_accumulator = ResponsesStreamAccumulator::default();
        let incomplete = incomplete_accumulator
            .push_value(&json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_incomplete_without_usage",
                    "incomplete_details": { "reason": "max_output_tokens" }
                }
            }))
            .expect("incomplete usage may be absent");
        assert!(matches!(
            incomplete.terminal,
            Some(ResponsesTerminal::Incomplete { usage: None, .. })
        ));
    }

    #[test]
    fn malformed_present_usage_is_rejected_and_completed_reconciliation_rolls_back() {
        let overflow = u64::from(u32::MAX) + 1;
        let malformed_usage = [
            Value::Null,
            json!([]),
            json!({
                "input_tokens": 1,
                "output_tokens": 2
            }),
            json!({
                "input_tokens": -1,
                "output_tokens": 2,
                "total_tokens": 3
            }),
            json!({
                "input_tokens": overflow,
                "output_tokens": 2,
                "total_tokens": overflow
            }),
            json!({
                "input_tokens": 1,
                "output_tokens": 2,
                "total_tokens": 3,
                "output_tokens_details": []
            }),
            json!({
                "input_tokens": 1,
                "output_tokens": 2,
                "total_tokens": 3,
                "output_tokens_details": { "reasoning_tokens": "1" }
            }),
        ];

        for usage in malformed_usage {
            let mut accumulator = ResponsesStreamAccumulator::default();
            let before = accumulator_state_snapshot(&accumulator);
            let error = accumulator
                .push_value(&json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_malformed_usage",
                        "output": [{
                            "type": "message",
                            "id": "msg_before_usage_error",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": "must roll back" }]
                        }],
                        "usage": usage
                    }
                }))
                .expect_err("present malformed usage must be rejected");
            assert!(error.to_string().contains("usage"));
            assert_eq!(accumulator_state_snapshot(&accumulator), before);
        }

        let mut incomplete_accumulator = ResponsesStreamAccumulator::default();
        let error = incomplete_accumulator
            .push_value(&json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_incomplete_malformed_usage",
                    "usage": { "input_tokens": 1 }
                }
            }))
            .expect_err("incomplete malformed usage must be rejected");
        assert!(error.to_string().contains("usage"));
        assert!(incomplete_accumulator.terminal().is_none());
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
