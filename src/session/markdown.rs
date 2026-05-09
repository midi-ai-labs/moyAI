use serde_json::Value;

use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload, ToolLifecycleStatus};
use crate::session::{
    MessageMetadata, MessagePart, MessageRole, PartRecord, RequestDiagnosticsPart, SessionId,
    ToolCallStatus, Transcript, TranscriptMessage,
};

pub fn transcript_to_markdown(transcript: &Transcript) -> String {
    let session = &transcript.session;
    let mut output = String::new();
    output.push_str("# ");
    output.push_str(&session.title);
    output.push_str("\n\n");
    push_metadata_line(&mut output, "Session ID", &session.id.to_string());
    push_metadata_line(&mut output, "Status", &format!("{:?}", session.status));
    push_metadata_line(&mut output, "Workspace", session.cwd.as_str());
    push_metadata_line(&mut output, "Model", &session.model);
    push_metadata_line(&mut output, "Base URL", &session.base_url);
    push_metadata_line(
        &mut output,
        "Created At (ms)",
        &session.created_at_ms.to_string(),
    );
    push_metadata_line(
        &mut output,
        "Updated At (ms)",
        &session.updated_at_ms.to_string(),
    );
    if let Some(completed_at_ms) = session.completed_at_ms {
        push_metadata_line(
            &mut output,
            "Completed At (ms)",
            &completed_at_ms.to_string(),
        );
    }
    output.push('\n');

    for message in &transcript.messages {
        push_message(&mut output, message);
    }

    output
}

pub fn history_items_to_markdown(
    session: &crate::session::SessionRecord,
    items: &[HistoryItem],
) -> String {
    let mut output = String::new();
    output.push_str("# ");
    output.push_str(&session.title);
    output.push_str("\n\n");
    push_metadata_line(&mut output, "Session ID", &session.id.to_string());
    push_metadata_line(&mut output, "Status", &format!("{:?}", session.status));
    push_metadata_line(&mut output, "Workspace", session.cwd.as_str());
    push_metadata_line(&mut output, "Model", &session.model);
    push_metadata_line(&mut output, "Base URL", &session.base_url);
    push_metadata_line(
        &mut output,
        "Created At (ms)",
        &session.created_at_ms.to_string(),
    );
    push_metadata_line(
        &mut output,
        "Updated At (ms)",
        &session.updated_at_ms.to_string(),
    );
    if let Some(completed_at_ms) = session.completed_at_ms {
        push_metadata_line(
            &mut output,
            "Completed At (ms)",
            &completed_at_ms.to_string(),
        );
    }
    output.push('\n');

    for item in items {
        push_history_item(&mut output, item);
    }

    output
}

pub fn history_markdown_file_name(title: &str, session_id: SessionId) -> String {
    let slug = title_slug(title);
    let id = session_id.to_string();
    let short_id = id.get(..10).unwrap_or(&id);
    format!("moyai-history-{slug}-{short_id}.md")
}

fn push_message(output: &mut String, message: &TranscriptMessage) {
    let role = match message.record.role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
    };
    output.push_str("## ");
    output.push_str(role);
    output.push_str(" Message ");
    output.push_str(&message.record.sequence_no.to_string());
    output.push_str("\n\n");

    if message.parts.is_empty() {
        output.push_str("_No recorded parts._\n\n");
        return;
    }

    for part in &message.parts {
        if let Some(payload) = materialized_history_payload_for_part(message, part) {
            push_history_payload(output, &payload);
            continue;
        }
        match &part.payload {
            MessagePart::Error(value) => {
                output.push_str("### Error\n\n");
                output.push_str("- Category: `");
                output.push_str(&format!("{:?}", value.category));
                output.push_str("`\n\n");
                output.push_str(&value.message);
                output.push_str("\n\n");
            }
            MessagePart::DiffSummary(value) => {
                output.push_str("### Diff Summary\n\n");
                output.push_str(&value.summary);
                output.push_str("\n\n");
            }
            MessagePart::PromptDispatch(value) => {
                output.push_str("### Prompt Dispatch\n\n");
                push_labeled_text(output, "Raw prompt", &value.raw_prompt_text);
                push_labeled_text(output, "Dispatched prompt", &value.dispatch_prompt_text);
                if let Some(draft) = &value.enhanced_draft_text {
                    push_labeled_text(output, "Enhanced draft", draft);
                }
                if !value.transforms.is_empty() {
                    output.push_str("Transforms:\n");
                    for transform in &value.transforms {
                        output.push_str("- `");
                        output.push_str(&format!("{:?}", transform.kind));
                        output.push('`');
                        if let Some(label) = &transform.label {
                            output.push_str(": ");
                            output.push_str(label);
                        }
                        output.push('\n');
                    }
                    output.push('\n');
                }
                if let Some(error) = &value.transform_error {
                    push_labeled_text(output, "Transform error", error);
                }
            }
            MessagePart::RequestDiagnostics(value) => {
                push_request_diagnostics(output, value);
            }
            MessagePart::Text(_)
            | MessagePart::Image(_)
            | MessagePart::Reasoning(_)
            | MessagePart::ToolCall(_)
            | MessagePart::ToolResult(_) => {}
        }
    }
}

fn push_history_item(output: &mut String, item: &HistoryItem) {
    let role = match &item.payload {
        HistoryItemPayload::UserTurn { .. } => Some("User"),
        HistoryItemPayload::Message { role, .. } => Some(match role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
        }),
        _ => None,
    };
    if let Some(role) = role {
        output.push_str("## ");
        output.push_str(role);
        output.push_str(" Message ");
        output.push_str(&item.sequence_no.to_string());
        output.push_str("\n\n");
    }
    push_history_payload(output, &item.payload);
}

fn materialized_history_payload_for_part(
    message: &TranscriptMessage,
    part: &PartRecord,
) -> Option<HistoryItemPayload> {
    match &part.payload {
        MessagePart::Text(value) => Some(HistoryItemPayload::Message {
            message_id: Some(message.record.id),
            role: message.record.role,
            content: vec![ContentPart::Text {
                text: value.text.clone(),
            }],
        }),
        MessagePart::Image(value) => Some(HistoryItemPayload::Message {
            message_id: Some(message.record.id),
            role: message.record.role,
            content: vec![ContentPart::Image {
                image: value.clone(),
            }],
        }),
        MessagePart::Reasoning(value) => Some(HistoryItemPayload::Reasoning {
            text: value.text.clone(),
        }),
        MessagePart::ToolCall(value) => Some(HistoryItemPayload::ToolCall {
            call_id: value.tool_call_id,
            tool: value.tool_name.clone(),
            arguments: serde_json::from_str(&value.arguments_json)
                .unwrap_or_else(|_| Value::String(value.arguments_json.clone())),
            model_arguments: value
                .model_arguments_json
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null),
            effective_arguments: value
                .effective_arguments_json
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_else(|| {
                    serde_json::from_str(&value.arguments_json)
                        .unwrap_or_else(|_| Value::String(value.arguments_json.clone()))
                }),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: Vec::new(),
            retry_policy: None,
            terminal_guard_policy: None,
        }),
        MessagePart::ToolResult(value) => Some(HistoryItemPayload::ToolOutput {
            call_id: value.tool_call_id,
            status: tool_lifecycle_status_from_session_status(value.status),
            title: value.title.clone(),
            output_text: value.summary.clone(),
            metadata: Value::Null,
            success: value.success,
            progress_effect: value.progress_effect.clone(),
            blocked_action: value.blocked_action.clone(),
            required_next_action: value.required_next_action.clone(),
            result_hash: value.result_hash.clone(),
            verification_run: None,
        }),
        MessagePart::RequestDiagnostics(value) => Some(HistoryItemPayload::RequestDiagnostics {
            diagnostics: value.clone(),
        }),
        MessagePart::DiffSummary(value) => Some(HistoryItemPayload::FileChange {
            change_ids: value.change_ids.clone(),
            changes: value.changes.clone(),
            summary: value.summary.clone(),
        }),
        MessagePart::PromptDispatch(value) => Some(HistoryItemPayload::PromptDispatch {
            dispatch: value.clone(),
            editor_context: match &message.record.metadata {
                MessageMetadata::User(meta) => meta.editor_context.clone(),
                _ => None,
            },
        }),
        MessagePart::Error(value) => Some(HistoryItemPayload::Error {
            message_id: Some(message.record.id),
            message: value.message.clone(),
        }),
    }
}

fn push_history_payload(output: &mut String, payload: &HistoryItemPayload) {
    match payload {
        HistoryItemPayload::UserTurn {
            content,
            prompt_dispatch,
            editor_context,
            turn_context,
            ..
        } => {
            push_content_parts(output, content);
            if let Some(prompt_dispatch) = prompt_dispatch {
                output.push_str("### Prompt Dispatch\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(prompt_dispatch).unwrap_or_default(),
                );
            }
            if let Some(editor_context) = editor_context {
                output.push_str("### Editor Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(editor_context).unwrap_or_default(),
                );
            }
            if let Some(turn_context) = turn_context {
                output.push_str("### Turn Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(turn_context).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::Message { content, .. } => {
            push_content_parts(output, content);
        }
        HistoryItemPayload::Error { message, .. } => {
            output.push_str("### Error\n\n");
            output.push_str(message);
            output.push_str("\n\n");
        }
        HistoryItemPayload::Reasoning { text } => {
            output.push_str("### Reasoning\n\n");
            output.push_str(text);
            output.push_str("\n\n");
        }
        HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            model_arguments,
            effective_arguments,
            adjusted_arguments,
            permission_decision,
            sandbox_decision,
            allowed_surface,
            retry_policy,
            terminal_guard_policy,
        } => {
            output.push_str("### Tool Call: ");
            output.push_str(&tool.to_string());
            output.push_str("\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n\n");
            let rendered =
                serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
            push_fenced(output, "json", &rendered);
            if !model_arguments.is_null()
                || !effective_arguments.is_null()
                || adjusted_arguments.is_some()
            {
                output.push_str("#### Tool Arguments Projection\n\n");
                let projection = serde_json::json!({
                    "model_arguments": model_arguments,
                    "effective_arguments": effective_arguments,
                    "adjusted_arguments": adjusted_arguments,
                });
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(&projection).unwrap_or_default(),
                );
            }
            if permission_decision.is_some()
                || sandbox_decision.is_some()
                || !allowed_surface.is_empty()
                || retry_policy.is_some()
                || terminal_guard_policy.is_some()
            {
                output.push_str("#### Tool Lifecycle Decisions\n\n");
                let decisions = serde_json::json!({
                    "permission_decision": permission_decision,
                    "sandbox_decision": sandbox_decision,
                    "allowed_surface": allowed_surface,
                    "retry_policy": retry_policy,
                    "terminal_guard_policy": terminal_guard_policy,
                });
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(&decisions).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            verification_run,
            success,
            progress_effect,
            blocked_action,
            required_next_action,
            result_hash,
            ..
        } => {
            output.push_str("### Tool Result: ");
            output.push_str(title);
            output.push_str("\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n");
            output.push_str("- Status: `");
            output.push_str(&format!("{status:?}"));
            output.push_str("`\n\n");
            if let Some(success) = success {
                output.push_str("- Success: `");
                output.push_str(&success.to_string());
                output.push_str("`\n");
            }
            output.push_str("- Progress effect: `");
            output.push_str(&format!("{progress_effect:?}"));
            output.push_str("`\n");
            let _ = (blocked_action, required_next_action);
            if let Some(hash) = result_hash {
                output.push_str("- Result hash: `");
                output.push_str(hash);
                output.push_str("`\n");
            }
            output.push('\n');
            output.push_str(output_text);
            output.push_str("\n\n");
            if let Some(verification_run) = verification_run {
                output.push_str("#### Verification Run\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(verification_run).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::RequestDiagnostics { diagnostics } => {
            push_request_diagnostics(output, diagnostics);
        }
        HistoryItemPayload::PromptDispatch {
            dispatch,
            editor_context,
        } => {
            output.push_str("### Prompt Dispatch\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(dispatch).unwrap_or_default(),
            );
            if let Some(editor_context) = editor_context {
                output.push_str("### Editor Context\n\n");
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(editor_context).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::FileChange {
            summary, changes, ..
        } => {
            output.push_str("### Diff Summary\n\n");
            output.push_str(summary);
            output.push_str("\n\n");
            if !changes.is_empty() {
                push_fenced(
                    output,
                    "json",
                    &serde_json::to_string_pretty(changes).unwrap_or_default(),
                );
            }
        }
        HistoryItemPayload::RejectedToolProposal { proposal } => {
            output.push_str("### Rejected Tool Proposal\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(proposal).unwrap_or_default(),
            );
        }
        HistoryItemPayload::CandidateRepairEdit { candidate } => {
            output.push_str("### Candidate Repair Edit\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(candidate).unwrap_or_default(),
            );
        }
        HistoryItemPayload::Continuation { contract } => {
            output.push_str("### Continuation\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(contract).unwrap_or_default(),
            );
        }
        HistoryItemPayload::StateProjection { projection } => {
            output.push_str("### State Projection\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(projection).unwrap_or_default(),
            );
        }
        HistoryItemPayload::SessionState { state } => {
            output.push_str("### Session State\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(state).unwrap_or_default(),
            );
        }
        HistoryItemPayload::ApprovalDecision { call_id, decision } => {
            output.push_str("### Approval Decision\n\n");
            output.push_str("- Tool call ID: `");
            output.push_str(&call_id.to_string());
            output.push_str("`\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(decision).unwrap_or_default(),
            );
        }
        HistoryItemPayload::RetryDecision {
            attempt,
            message,
            next_retry_at_ms,
        } => {
            output.push_str("### Retry Decision\n\n");
            output.push_str("- Attempt: `");
            output.push_str(&attempt.to_string());
            output.push_str("`\n");
            output.push_str("- Next retry at ms: `");
            output.push_str(&next_retry_at_ms.to_string());
            output.push_str("`\n\n");
            output.push_str(message);
            output.push_str("\n\n");
        }
        HistoryItemPayload::ControlEnvelope { envelope } => {
            output.push_str("### Control Envelope\n\n");
            push_fenced(
                output,
                "json",
                &serde_json::to_string_pretty(envelope).unwrap_or_default(),
            );
        }
        HistoryItemPayload::Compaction { summary, .. } => {
            output.push_str("### Compaction\n\n");
            output.push_str(summary);
            output.push_str("\n\n");
        }
    }
}

fn push_content_parts(output: &mut String, content: &[ContentPart]) {
    for content in content {
        match content {
            ContentPart::Text { text } => {
                output.push_str(text);
                output.push_str("\n\n");
            }
            ContentPart::Image { image } => {
                output.push_str("### Image Attachment\n\n");
                if let Some(path) = &image.source_path {
                    push_metadata_line(output, "Source", path.as_str());
                }
                push_metadata_line(output, "MIME Type", &image.mime_type);
                push_metadata_line(output, "Bytes", &image.byte_len.to_string());
                output.push('\n');
            }
        }
    }
}

fn tool_lifecycle_status_from_session_status(status: ToolCallStatus) -> ToolLifecycleStatus {
    match status {
        ToolCallStatus::Pending => ToolLifecycleStatus::Pending,
        ToolCallStatus::Running => ToolLifecycleStatus::Running,
        ToolCallStatus::Completed => ToolLifecycleStatus::Completed,
        ToolCallStatus::Failed => ToolLifecycleStatus::Failed,
    }
}

fn push_request_diagnostics(output: &mut String, value: &RequestDiagnosticsPart) {
    output.push_str("### Request Diagnostics\n\n");
    push_metadata_line(output, "Provider", &value.provider);
    push_metadata_line(output, "Model", &value.model_name);
    push_metadata_line(output, "Base URL", &value.base_url);
    push_metadata_line(
        output,
        "Request Timeout (ms)",
        &value.request_timeout_ms.to_string(),
    );
    push_metadata_line(
        output,
        "Stream Idle Timeout (ms)",
        &value.stream_idle_timeout_ms.to_string(),
    );
    push_metadata_line(output, "Tool Count", &value.tool_count.to_string());
    push_metadata_line(
        output,
        "Provider Message Count",
        &value.provider_message_count.to_string(),
    );
    if value.image_count > 0 {
        push_metadata_line(output, "Image Count", &value.image_count.to_string());
        push_metadata_line(output, "Image Bytes", &value.image_bytes.to_string());
    }
    if !value.tool_names.is_empty() {
        push_metadata_line(output, "Tools", &value.tool_names.join(", "));
    }
    if let Some(control) = &value.control_envelope {
        push_metadata_line(output, "Control Projection ID", &control.projection_id);
        push_metadata_line(output, "Control Policy", &control.dispatch_policy);
        push_metadata_line(output, "Control Validation", &control.validation_status);
        if !control.allowed_tools.is_empty() {
            push_metadata_line(
                output,
                "Allowed Control Tools",
                &control.allowed_tools.join(", "),
            );
        }
    }
    output.push('\n');
}

fn push_labeled_text(output: &mut String, label: &str, value: &str) {
    output.push_str("**");
    output.push_str(label);
    output.push_str("**\n\n");
    output.push_str(value);
    output.push_str("\n\n");
}

fn push_metadata_line(output: &mut String, label: &str, value: &str) {
    output.push_str("- ");
    output.push_str(label);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn push_fenced(output: &mut String, language: &str, value: &str) {
    let fence = markdown_fence_for(value);
    output.push_str(&fence);
    if !language.is_empty() {
        output.push_str(language);
    }
    output.push('\n');
    output.push_str(value);
    if !value.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&fence);
    output.push_str("\n\n");
}

fn markdown_fence_for(value: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in value.chars() {
        if ch == '`' {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(max_run.max(2) + 1)
}

fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}
