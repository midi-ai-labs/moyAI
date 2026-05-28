use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use serde_json::{Value, json};

use crate::agent::grounding_evidence::{
    metadata_path_matches_active_target, normalize_path_for_target_match,
};
use crate::agent::lifecycle_kernel::TurnLifecycleKernel;
use crate::edit::ChangeSummary;
use crate::protocol::{
    CandidateRepairEdit, CandidateRepairId, CandidateRepairValidity, OperationIntent, ToolChoice,
    ToolProposalId,
};
use crate::session::{ProcessPhase, SessionStateSnapshot, TaskRoute};
use crate::tool::{ToolName, ToolResult};

const INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD: usize = 3;
#[derive(Debug, Clone)]
pub(crate) struct InvalidEditRecoveryEnvelope {
    pub(crate) failure_kind: String,
    pub(crate) tool_name: String,
    pub(crate) active_targets: Vec<String>,
    pub(crate) candidate_target: Option<String>,
    pub(crate) submitted_targets: Vec<String>,
    pub(crate) active_submitted_targets: Vec<String>,
    pub(crate) inactive_submitted_targets: Vec<String>,
    pub(crate) parser_error_family: Option<String>,
    pub(crate) recovery_action: Option<String>,
    pub(crate) recovery_target: Option<String>,
    pub(crate) result_hash: Option<String>,
    pub(crate) prompt: String,
}

pub(crate) fn invalid_edit_arguments_control_recovery_envelope(
    tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> Option<InvalidEditRecoveryEnvelope> {
    let envelope = failed_edit_control_recovery_envelope(
        tool_name,
        metadata,
        state,
        allowed_tools,
        tool_choice,
    )?;
    (envelope.failure_kind == "invalid_edit_arguments").then_some(envelope)
}

pub(crate) fn failed_edit_control_recovery_envelope(
    tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> Option<InvalidEditRecoveryEnvelope> {
    let feedback = metadata.get("tool_feedback_envelope")?;
    let failure_kind = feedback.get("kind").and_then(Value::as_str)?;
    if !matches!(
        failure_kind,
        "invalid_edit_arguments" | "required_write_content_shape_mismatch"
    ) || feedback.get("progress_effect").and_then(Value::as_str) != Some("no_progress")
        || feedback
            .get("side_effects_applied")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return None;
    }
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect::<Vec<_>>();
    let target_text = if active_targets.is_empty() {
        "none recorded".to_string()
    } else {
        active_targets.join(", ")
    };
    let parser_error = feedback
        .get("parser_error")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("parser_error").and_then(Value::as_str))
        .or_else(|| metadata.get("error").and_then(Value::as_str))
        .unwrap_or_else(|| {
            if failure_kind == "required_write_content_shape_mismatch" {
                "required write content shape mismatch"
            } else {
                "invalid edit arguments"
            }
        });
    let parser_error_family = feedback
        .get("parser_error_family")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("parser_error_family").and_then(Value::as_str))
        .or_else(|| {
            if failure_kind == "required_write_content_shape_mismatch" {
                feedback
                    .get("content_shape_contract")
                    .or_else(|| metadata.get("content_shape_contract"))
                    .and_then(|contract| contract.get("kind"))
                    .and_then(Value::as_str)
            } else {
                None
            }
        })
        .or_else(|| {
            (failure_kind == "required_write_content_shape_mismatch")
                .then_some("required_write_content_shape_mismatch")
        })
        .map(str::to_string);
    let candidate_target = feedback
        .get("candidate_target_from_arguments")
        .and_then(Value::as_str)
        .or_else(|| {
            metadata
                .get("candidate_target_from_arguments")
                .and_then(Value::as_str)
        })
        .or_else(|| feedback.get("target").and_then(Value::as_str))
        .or_else(|| metadata.get("target").and_then(Value::as_str))
        .map(str::to_string);
    let submitted_targets = feedback
        .get("submitted_targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| candidate_target.iter().cloned().collect());
    let active_target_keys = active_targets
        .iter()
        .map(|target| normalize_path_for_target_match(target))
        .collect::<BTreeSet<_>>();
    let mut active_submitted_targets = Vec::<String>::new();
    let mut inactive_submitted_targets = Vec::<String>::new();
    for target in submitted_targets.iter() {
        let normalized = normalize_path_for_target_match(target);
        if active_target_keys.contains(&normalized) {
            active_submitted_targets.push(target.clone());
        } else {
            inactive_submitted_targets.push(target.clone());
        }
    }
    let mixed_patch_target_evidence = tool_name == "apply_patch"
        && !active_submitted_targets.is_empty()
        && !inactive_submitted_targets.is_empty();
    let candidate_target_is_open = candidate_target
        .as_ref()
        .is_some_and(|target| active_targets.iter().any(|active| active == target));
    let recovery_target = if active_submitted_targets.len() == 1 {
        active_submitted_targets.first().cloned()
    } else if candidate_target_is_open {
        candidate_target.clone()
    } else if active_targets.len() == 1 {
        active_targets.first().cloned()
    } else {
        None
    };
    let recovery_target_text = recovery_target
        .as_deref()
        .map(|target| format!("`{target}`"))
        .unwrap_or_else(|| "one open target".to_string());
    let candidate_target_line = if mixed_patch_target_evidence {
        format!(
            "Submitted patch declared active target(s) `{}` but also inactive target(s) `{}`. Runtime rejected the whole patch before side effects; resend a target-only edit for the active target and do not include inactive source hunks.",
            active_submitted_targets.join(", "),
            inactive_submitted_targets.join(", ")
        )
    } else if failure_kind == "required_write_content_shape_mismatch" {
        let contract_kind = feedback
            .get("content_shape_contract")
            .and_then(|contract| contract.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("content_shape_contract");
        let observed = feedback
            .get("observed_forbidden_markers")
            .or_else(|| metadata.get("observed_forbidden_markers"))
            .and_then(Value::as_array)
            .map(|markers| {
                markers
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|marker| format!("`{marker}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|markers| !markers.is_empty())
            .map(|markers| format!(" Observed rejected markers: {markers}."))
            .unwrap_or_default();
        match candidate_target.as_ref() {
            Some(target) => format!(
                "Latest attempted edit target: `{target}`. Runtime rejected the submitted content before filesystem side effects because it violates `{contract_kind}`.{observed} Rewrite the content for the same active target with the required positive artifact shape."
            ),
            None => format!(
                "Latest attempted edit target: none recorded. Runtime rejected the submitted content before filesystem side effects because it violates `{contract_kind}`.{observed} Choose one open target and submit content with the required positive artifact shape."
            ),
        }
    } else {
        match candidate_target.as_ref() {
            Some(target) if candidate_target_is_open => format!(
                "Latest attempted edit target: `{target}`. This target is still open; retry the same bounded edit operation for `{target}` with corrected arguments before moving to another target."
            ),
            Some(target) => format!(
                "Latest attempted edit target: `{target}`. It is not currently an open target, so choose one of the open targets instead of repeating stale arguments."
            ),
            None => "Latest attempted edit target: none recorded; choose one open target and submit a corrected edit call.".to_string(),
        }
    };
    let allowed = if allowed_tools.is_empty() {
        "none".to_string()
    } else {
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let grammar_line = if tool_name == "apply_patch" || allowed_tools.contains("apply_patch") {
        if tool_name == "apply_patch"
            && parser_error_family.as_deref() == Some("apply_patch_malformed_patch")
        {
            "Use the exact apply_patch grammar. Add File body lines must start with `+`, including blank lines and top-level `def`/`class`/`import` lines. Update File hunks must use `@@` and every hunk line must start with ` `, `+`, or `-`; a single patch may contain multiple `*** Add File` or `*** Update File` sections. If the OpenAI-compatible function-tool payload keeps corrupting `patch_text` line prefixes, use `write` with complete content for the current open target when the recovery surface provides `write`."
        } else {
            "Use the exact apply_patch grammar. Add File body lines must start with `+`, including blank lines and top-level `def`/`class`/`import` lines. Update File hunks must use `@@` and every hunk line must start with ` `, `+`, or `-`; a single patch may contain multiple `*** Add File` or `*** Update File` sections."
        }
    } else {
        "Use a schema-valid edit call for the active target. Do not treat malformed edit output, planning, or text-only responses as progress."
    };
    let content_shape_contract = if failure_kind == "required_write_content_shape_mismatch" {
        metadata
            .get("content_shape_contract")
            .or_else(|| feedback.get("content_shape_contract"))
            .map(|contract| format!("\nActive content-shape contract:\n{contract}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let result_hash = feedback
        .get("result_hash")
        .and_then(Value::as_str)
        .map(str::to_string);
    let recovery_action = feedback
        .get("recovery_action")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            (failure_kind == "required_write_content_shape_mismatch")
                .then(|| "rewrite_content_for_required_shape".to_string())
        });
    let active_test_contract = if active_targets.len() == 1 {
        crate::agent::content_shape_contract::python_source_for_test_target(&active_targets[0])
            .map(|contract| {
                format!(
                    "\nActive generated-test target contract:\n{}",
                    contract.prompt_contract()
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let prompt = format!(
        "The latest `{tool_name}` edit output was rejected before filesystem side effects and is no-progress evidence, not authoring progress.\nFailure kind: {failure_kind}.\nOpen targets: {target_text}.\n{candidate_target_line}\nAllowed tools for recovery: {allowed}. Tool choice remains `{}` unless the TurnControlEnvelope says otherwise.\nLatest parser/content-shape error: {parser_error}\n{grammar_line}{active_test_contract}{content_shape_contract}\nRequired recovery operation: submit a corrected `{tool_name}` content-changing edit for {recovery_target_text} before any verification, progress-only todo update, or final answer.",
        tool_choice_label(tool_choice)
    );
    Some(InvalidEditRecoveryEnvelope {
        failure_kind: failure_kind.to_string(),
        tool_name: tool_name.to_string(),
        active_targets,
        candidate_target,
        submitted_targets,
        active_submitted_targets,
        inactive_submitted_targets,
        parser_error_family,
        recovery_action,
        recovery_target,
        result_hash,
        prompt,
    })
}

pub(crate) fn invalid_tool_arguments_result(
    tool_name: &str,
    arguments_json: &str,
    error: &str,
    state: &SessionStateSnapshot,
    allowed_tools: Option<&BTreeSet<String>>,
    tool_choice: Option<&ToolChoice>,
) -> ToolResult {
    let open_authoring = TurnLifecycleKernel::open_executable_work_requires_tool_call(state);
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect::<Vec<_>>();
    let mut output_text = format!(
        "Invalid arguments for `{tool_name}`: {error}. Please rewrite the input so it satisfies the expected schema."
    );
    let mut metadata = json!({
        "invalid_tool_arguments": true,
        "tool_name": tool_name,
        "arguments_json": arguments_json,
        "error": error,
        "side_effects_applied": false,
    });
    if open_authoring && matches!(tool_name, "write" | "apply_patch") {
        let raw_argument_shape =
            normalized_tool_arguments_for_invalid_edit_hash(arguments_json, error);
        let raw_argument_shape_hash = malformed_edit_argument_raw_shape_hash(&raw_argument_shape);
        let submitted_targets = submitted_targets_from_invalid_edit_arguments(arguments_json);
        let candidate_target = submitted_targets
            .first()
            .cloned()
            .or_else(|| candidate_target_from_invalid_edit_arguments(arguments_json));
        let parser_error_family = invalid_edit_parser_error_family(error);
        let allowed_surface_snapshot =
            invalid_edit_allowed_surface_snapshot(allowed_tools, tool_choice);
        let result_hash = crate::harness::artifact::hash_bytes(
            format!(
                "invalid_edit_arguments|{tool_name}|{}|{}|{}",
                raw_argument_shape,
                invalid_edit_error_hash_component(error),
                active_targets.join(",")
            )
            .as_bytes(),
        );
        let active_target_line = if active_targets.is_empty() {
            "Open active targets remain, but no exact target is recorded.".to_string()
        } else {
            format!("Open active target(s): {}.", active_targets.join(", "))
        };
        let patch_context_mismatch =
            tool_name == "apply_patch" && is_apply_patch_context_mismatch_error(error);
        let write_available = allowed_tools.is_some_and(|tools| tools.contains("write"));
        let active_target_keys = active_targets
            .iter()
            .map(|target| normalize_path_for_target_match(target))
            .collect::<BTreeSet<_>>();
        let active_submitted_targets = submitted_targets
            .iter()
            .filter(|target| active_target_keys.contains(&normalize_path_for_target_match(target)))
            .cloned()
            .collect::<Vec<_>>();
        let inactive_submitted_targets = submitted_targets
            .iter()
            .filter(|target| !active_target_keys.contains(&normalize_path_for_target_match(target)))
            .cloned()
            .collect::<Vec<_>>();
        let mixed_patch_target_evidence = tool_name == "apply_patch"
            && !active_submitted_targets.is_empty()
            && !inactive_submitted_targets.is_empty();
        let recovery_action = if patch_context_mismatch && write_available {
            "write_full_replacement_or_repatch_after_patch_context_mismatch"
        } else if patch_context_mismatch {
            "target_scoped_inspection_then_repatch_after_patch_context_mismatch"
        } else if mixed_patch_target_evidence {
            "mixed_target_apply_patch_rewrite_target_only"
        } else {
            "correct_edit_call_for_active_target"
        };
        let recovery_line = if mixed_patch_target_evidence {
            format!(
                "The patch was rejected before filesystem side effects because it declared active target(s) `{}` together with inactive target(s) `{}`. Resend a target-only `apply_patch` for the active target, or use `write` with complete content for the active target if available. Do not include inactive source hunks in this recovery edit.",
                active_submitted_targets.join(", "),
                inactive_submitted_targets.join(", ")
            )
        } else if patch_context_mismatch {
            if write_available && active_targets.len() == 1 {
                format!(
                    "The patch was rejected before filesystem side effects because its context did not match the current file. Use `write` with `path` set to `{}` and complete replacement content, or resend an `apply_patch` hunk anchored to exact lines from the latest read. Do not repeat the same read-only probe as progress.",
                    active_targets[0]
                )
            } else if write_available {
                "The patch was rejected before filesystem side effects because its context did not match the current file. Use `write` with `path` set to the active target you are repairing and complete replacement content, or resend an `apply_patch` hunk anchored to exact lines from the latest read. Do not repeat the same read-only probe as progress.".to_string()
            } else if active_targets.len() == 1 {
                format!(
                    "The patch was rejected before filesystem side effects because its context did not match the current file. If current contents are needed, inspect only the exact active target `{}` with `shell`, then resend an `apply_patch` hunk anchored to exact current lines. Do not inspect unrelated files or run verification before the repair edit.",
                    active_targets[0]
                )
            } else {
                "The patch was rejected before filesystem side effects because its context did not match the current file. If current contents are needed, inspect only the exact active repair target with `shell`, then resend an `apply_patch` hunk anchored to exact current lines. Do not inspect unrelated files or run verification before the repair edit.".to_string()
            }
        } else if tool_name == "write" && is_malformed_json_string_eof(error) {
            if write_available {
                "The edit was rejected before filesystem side effects because the `write` arguments were malformed or truncated. Continue with a smaller valid JSON `write` call for the active target, or use `apply_patch` with a concise add/update patch. Keep generated files focused on the requested public behavior; do not add unrelated API tests just to increase coverage.".to_string()
            } else {
                "The edit was rejected before filesystem side effects because the omitted `write` tool arguments were malformed or truncated. Continue with `apply_patch` for the exact active target; do not retry whole-file `write` in this Code lifecycle.".to_string()
            }
        } else if tool_name == "apply_patch"
            && parser_error_family == "apply_patch_malformed_patch"
            && matches!(state.route, TaskRoute::Code)
        {
            "The patch was rejected before filesystem side effects because the submitted patch text did not satisfy the freeform patch grammar. Correct the `apply_patch` grammar for the active target. If the next recovery surface includes `write`, use `write` with complete file content for the latest attempted open target instead of repeating malformed JSON-wrapped patch text.".to_string()
        } else {
            if write_available {
                "The edit was rejected before filesystem side effects. Continue with a corrected `write` or `apply_patch` call for the active target before verification or final answer.".to_string()
            } else {
                "The edit was rejected before filesystem side effects. Continue with a corrected `apply_patch` call for the active target before verification or final answer.".to_string()
            }
        };
        output_text = format!(
            "{output_text}\n\n[tool feedback]\noperation_intent: content_changing_authoring_required\noperation_progress_class: invalid_edit_arguments\nprogress_effect: no_progress\nsubmitted_tool: {tool_name}\nparser_error_family: {parser_error_family}\nraw_argument_shape_hash: {raw_argument_shape_hash}\ncandidate_target_from_arguments: {}\nactive_targets: {}\n{active_target_line}\n{recovery_line}",
            candidate_target.as_deref().unwrap_or("unknown"),
            active_targets.join(", ")
        );
        metadata = json!({
            "invalid_tool_arguments": true,
            "tool_name": tool_name,
            "arguments_json": arguments_json,
            "error": error,
            "parser_error": error,
            "parser_error_family": parser_error_family,
            "submitted_tool": tool_name,
            "candidate_target_from_arguments": candidate_target,
            "submitted_targets": submitted_targets.clone(),
            "active_submitted_targets": active_submitted_targets.clone(),
            "inactive_submitted_targets": inactive_submitted_targets.clone(),
            "raw_argument_shape": raw_argument_shape,
            "raw_argument_shape_hash": raw_argument_shape_hash,
            "allowed_surface_snapshot": allowed_surface_snapshot,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "invalid_edit_arguments",
            "progress_effect": "no_progress",
            "active_targets": active_targets.clone(),
            "result_hash": result_hash.clone(),
            "side_effects_applied": false,
            "tool_feedback_envelope": {
                "kind": "invalid_edit_arguments",
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "invalid_edit_arguments",
                "progress_effect": "no_progress",
                "active_targets": state.active_targets.iter().map(|target| target.as_str().to_string()).collect::<Vec<_>>(),
                "submitted_tool": tool_name,
                "parser_error": error,
                "parser_error_family": parser_error_family,
                "candidate_target_from_arguments": metadata_candidate_target(arguments_json),
                "submitted_targets": metadata_submitted_targets(arguments_json),
                "active_submitted_targets": active_submitted_targets.clone(),
                "inactive_submitted_targets": inactive_submitted_targets.clone(),
                "raw_argument_shape_hash": raw_argument_shape_hash,
                "allowed_surface_snapshot": allowed_surface_snapshot,
                "result_hash": result_hash,
                "side_effects_applied": false,
                "recovery_action": recovery_action
            }
        });
    }
    ToolResult {
        title: "Invalid tool arguments".to_string(),
        output_text,
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::<ChangeSummary>::new(),
    }
}

fn malformed_edit_argument_raw_shape_hash(raw_argument_shape: &str) -> String {
    crate::harness::artifact::hash_bytes(
        format!("malformed_edit_argument_raw_shape|{raw_argument_shape}").as_bytes(),
    )
}

fn candidate_target_from_invalid_edit_arguments(arguments_json: &str) -> Option<String> {
    submitted_targets_from_invalid_edit_arguments(arguments_json)
        .into_iter()
        .next()
}

fn metadata_candidate_target(arguments_json: &str) -> Value {
    candidate_target_from_invalid_edit_arguments(arguments_json)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn metadata_submitted_targets(arguments_json: &str) -> Value {
    Value::Array(
        submitted_targets_from_invalid_edit_arguments(arguments_json)
            .into_iter()
            .map(Value::String)
            .collect(),
    )
}

fn submitted_targets_from_invalid_edit_arguments(arguments_json: &str) -> Vec<String> {
    let mut targets = Vec::<String>::new();
    for field in ["path", "file_path"] {
        if let Some(target) = extract_jsonish_string_field(arguments_json, field) {
            let normalized = target.replace('\\', "/");
            if !normalized.trim().is_empty() && !targets.contains(&normalized) {
                targets.push(normalized);
            }
        }
    }
    for target in extract_patch_targets_from_invalid_edit_arguments(arguments_json) {
        let normalized = target.replace('\\', "/");
        if !normalized.trim().is_empty() && !targets.contains(&normalized) {
            targets.push(normalized);
        }
    }
    targets
}

fn invalid_edit_allowed_surface_snapshot(
    allowed_tools: Option<&BTreeSet<String>>,
    tool_choice: Option<&ToolChoice>,
) -> Value {
    json!({
        "allowed_tools": allowed_tools
            .map(|tools| tools.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
        "tool_choice": tool_choice
            .map(tool_choice_label)
            .unwrap_or("unknown")
    })
}

pub(crate) fn invalid_write_arguments_need_patch_capable_recovery(
    effective_tool_name: &str,
    metadata: &Value,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> bool {
    if effective_tool_name != "write"
        || !allowed_tools.contains("write")
        || !allowed_tools.contains("apply_patch")
    {
        return false;
    }
    let Some(feedback) = metadata.get("tool_feedback_envelope") else {
        return false;
    };
    if feedback.get("kind").and_then(Value::as_str) != Some("invalid_edit_arguments")
        || feedback.get("submitted_tool").and_then(Value::as_str) != Some("write")
        || feedback.get("progress_effect").and_then(Value::as_str) != Some("no_progress")
        || feedback
            .get("side_effects_applied")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return false;
    }
    let parser_family = feedback
        .get("parser_error_family")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let eof_truncated_write = matches!(
        parser_family,
        "eof_while_parsing_string" | "eof_while_parsing_json"
    );
    let has_active_target = feedback
        .get("active_targets")
        .and_then(Value::as_array)
        .is_some_and(|targets| !targets.is_empty());
    eof_truncated_write
        && has_active_target
        && matches!(
            tool_choice,
            ToolChoice::Named(ToolName::Write) | ToolChoice::Required | ToolChoice::Auto
        )
}

pub(crate) fn invalid_apply_patch_arguments_need_write_recovery(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> bool {
    let route_allows_recovery = (matches!(state.route, TaskRoute::Code)
        && matches!(
            state.process_phase,
            crate::session::ProcessPhase::Author | crate::session::ProcessPhase::Repair
        ))
        || (matches!(state.route, TaskRoute::Docs)
            && matches!(state.process_phase, crate::session::ProcessPhase::Author));
    if effective_tool_name != "apply_patch"
        || !route_allows_recovery
        || !TurnLifecycleKernel::open_executable_work_requires_tool_call(state)
        || !allowed_tools.contains("apply_patch")
    {
        return false;
    }
    let Some(feedback) = metadata.get("tool_feedback_envelope") else {
        return false;
    };
    if feedback.get("kind").and_then(Value::as_str) != Some("invalid_edit_arguments")
        || feedback.get("submitted_tool").and_then(Value::as_str) != Some("apply_patch")
        || feedback.get("progress_effect").and_then(Value::as_str) != Some("no_progress")
        || feedback
            .get("side_effects_applied")
            .and_then(Value::as_bool)
            != Some(false)
        || feedback.get("parser_error_family").and_then(Value::as_str)
            != Some("apply_patch_malformed_patch")
    {
        return false;
    }
    let active_targets = feedback
        .get("active_targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(|target| target.replace('\\', "/"))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if active_targets.is_empty() {
        return false;
    }
    matches!(
        tool_choice,
        ToolChoice::Named(ToolName::ApplyPatch) | ToolChoice::Required | ToolChoice::Auto
    )
}

fn normalized_tool_arguments_for_invalid_edit_hash(arguments_json: &str, error: &str) -> String {
    serde_json::from_str::<Value>(arguments_json)
        .map(|value| {
            serde_json::to_string(&value).unwrap_or_else(|_| arguments_json.trim().to_string())
        })
        .unwrap_or_else(|_| {
            let family = invalid_json_error_family(error);
            let path = extract_jsonish_string_field(arguments_json, "path")
                .unwrap_or_else(|| "<unknown>".to_string());
            format!("malformed_json|family={family}|path={path}")
        })
}

fn invalid_json_error_family(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("eof while parsing a string") {
        "eof_while_parsing_string"
    } else if lower.contains("eof while parsing") {
        "eof_while_parsing_json"
    } else if lower.contains("expected") {
        "json_expected_token"
    } else {
        "malformed_json"
    }
}

fn invalid_edit_parser_error_family(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if is_apply_patch_context_mismatch_error(error) {
        "apply_patch_context_mismatch"
    } else if lower.contains("tool patch error") || lower.contains("add file body line") {
        "apply_patch_malformed_patch"
    } else {
        invalid_json_error_family(error)
    }
}

fn invalid_edit_error_hash_component(error: &str) -> String {
    if serde_json_error_like(error) {
        invalid_json_error_family(error).to_string()
    } else {
        error.to_ascii_lowercase()
    }
}

fn serde_json_error_like(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("while parsing")
        || lower.contains("expected")
        || lower.contains("trailing characters")
        || lower.contains("invalid type")
}

pub(crate) fn is_malformed_json_string_eof(error: &str) -> bool {
    invalid_json_error_family(error) == "eof_while_parsing_string"
}

fn extract_jsonish_string_field(arguments_json: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{field}\"");
    let start = arguments_json.find(&pattern)? + pattern.len();
    let after_colon = arguments_json[start..].find(':')? + start + 1;
    let mut chars = arguments_json[after_colon..].char_indices().peekable();
    while let Some((_, ch)) = chars.peek().copied() {
        if ch.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    let (_, opening) = chars.next()?;
    if opening != '"' {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    for (_, ch) in chars {
        if escaped {
            value.push(match ch {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }
    None
}

fn extract_patch_targets_from_invalid_edit_arguments(arguments_json: &str) -> Vec<String> {
    let Some(value) = serde_json::from_str::<Value>(arguments_json).ok() else {
        return Vec::new();
    };
    let Some(patch_text) = value.get("patch_text").and_then(Value::as_str) else {
        return Vec::new();
    };
    patch_text
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("*** Add File:")
                .or_else(|| line.trim().strip_prefix("*** Update File:"))
                .or_else(|| line.trim().strip_prefix("*** Delete File:"))
                .or_else(|| line.trim().strip_prefix("*** Move to:"))
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn is_apply_patch_context_mismatch_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("context mismatch")
        || lower.contains("failed to find expected lines")
        || lower.contains("could not find expected lines")
        || lower.contains("expected lines")
}

pub(crate) fn record_patch_context_mismatch_grounding_targets(
    targets: &mut BTreeSet<String>,
    metadata: &Value,
    state: &SessionStateSnapshot,
) {
    let verification_repair = state.process_phase == crate::session::ProcessPhase::Repair
        && state.completion.verification_pending;
    let docs_authoring = state.route == TaskRoute::Docs
        && state.process_phase == crate::session::ProcessPhase::Author
        && state.completion.open_work_count > 0;
    if !verification_repair && !docs_authoring {
        return;
    }
    let Some(feedback) = metadata.get("tool_feedback_envelope") else {
        return;
    };
    if feedback.get("kind").and_then(Value::as_str) != Some("invalid_edit_arguments")
        || !matches!(
            feedback.get("recovery_action").and_then(Value::as_str),
            Some(
                "write_full_replacement_or_repatch_after_patch_context_mismatch"
                    | "target_scoped_inspection_then_repatch_after_patch_context_mismatch"
            )
        )
    {
        return;
    }
    for target in state.active_targets.iter() {
        targets.insert(normalize_path_for_target_match(target.as_str()));
    }
}

pub(crate) fn patch_context_mismatch_target_grounding_surface_active(
    state: &SessionStateSnapshot,
    targets: &BTreeSet<String>,
) -> bool {
    let verification_repair = state.route != TaskRoute::Docs
        && state.process_phase == crate::session::ProcessPhase::Repair
        && state.completion.verification_pending;
    let docs_authoring = state.route == TaskRoute::Docs
        && state.process_phase == crate::session::ProcessPhase::Author
        && state.completion.open_work_count > 0;
    (verification_repair || docs_authoring)
        && !targets.is_empty()
        && state.active_targets.iter().any(|target| {
            let normalized = normalize_path_for_target_match(target.as_str());
            targets.iter().any(|recorded| {
                normalized == *recorded
                    || normalized.ends_with(&format!("/{recorded}"))
                    || recorded.ends_with(&format!("/{normalized}"))
            })
        })
}

pub(crate) fn patch_context_mismatch_target_grounding_read_satisfied(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> bool {
    let verification_repair = state.process_phase == crate::session::ProcessPhase::Repair
        && state.completion.verification_pending;
    let docs_authoring = state.route == TaskRoute::Docs
        && state.process_phase == crate::session::ProcessPhase::Author
        && state.completion.open_work_count > 0;
    effective_tool_name == "read"
        && (verification_repair || docs_authoring)
        && metadata_path_matches_active_target(metadata, state)
}

pub(crate) fn invalid_edit_arguments_no_progress_key(
    effective_tool_name: &str,
    metadata: &Value,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> Option<String> {
    if !matches!(effective_tool_name, "write" | "apply_patch") {
        return None;
    }
    let feedback = metadata.get("tool_feedback_envelope")?;
    if feedback.get("kind").and_then(Value::as_str) != Some("invalid_edit_arguments")
        || feedback.get("progress_effect").and_then(Value::as_str) != Some("no_progress")
        || feedback
            .get("side_effects_applied")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return None;
    }
    let parser_error_family = feedback
        .get("parser_error_family")
        .and_then(Value::as_str)
        .unwrap_or("unknown_parser_family");
    let candidate_target = feedback
        .get("candidate_target_from_arguments")
        .and_then(Value::as_str)
        .unwrap_or("missing_candidate_target");
    let active_targets = feedback
        .get("active_targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .join(",");
    Some(format!(
        "invalid_edit_arguments|tool={effective_tool_name}|parser_family={parser_error_family}|candidate_target={candidate_target}|targets={active_targets}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice)
    ))
}

pub(crate) fn invalid_tool_arguments_no_progress_key(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> Option<String> {
    if metadata
        .get("invalid_tool_arguments")
        .and_then(Value::as_bool)
        != Some(true)
        || metadata
            .get("side_effects_applied")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return None;
    }
    if matches!(effective_tool_name, "write" | "apply_patch")
        && invalid_edit_arguments_no_progress_key(
            effective_tool_name,
            metadata,
            allowed_tools,
            tool_choice,
        )
        .is_some()
    {
        return None;
    }
    let arguments_shape = invalid_tool_argument_shape(
        metadata
            .get("arguments_json")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let error_family = invalid_tool_argument_error_family(
        metadata
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_tool_argument_error"),
    );
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| normalize_path_for_target_match(target.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!(
        "invalid_tool_arguments|tool={effective_tool_name}|error_family={error_family}|argument_shape={arguments_shape}|targets={active_targets}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice)
    ))
}

pub(crate) fn invalid_tool_arguments_terminal_message(
    effective_tool_name: &str,
    count: usize,
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> String {
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>();
    let target_text = if active_targets.is_empty() {
        "open executable work".to_string()
    } else {
        format!("active target(s): {}", active_targets.join(", "))
    };
    let error = metadata
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("invalid tool arguments");
    format!(
        "Provider repeated invalid arguments for `{effective_tool_name}` with no progress {count} time(s) while {target_text} remained open. Last schema error: {error}. Runtime stopped before spending more turns on the same malformed supporting tool call."
    )
}

fn invalid_tool_argument_shape(arguments_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments_json) else {
        return "unparseable_json".to_string();
    };
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}:{}", value_type_name(value)))
            .collect::<Vec<_>>()
            .join(","),
        other => value_type_name(&other).to_string(),
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn invalid_tool_argument_error_family(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("invalid type") && lower.contains("expected usize") {
        return "invalid_type_expected_usize".to_string();
    }
    if lower.contains("invalid type") {
        return "invalid_type".to_string();
    }
    if lower.contains("missing field") {
        return "missing_required_field".to_string();
    }
    if lower.contains("unknown field") {
        return "unknown_field".to_string();
    }
    crate::harness::artifact::hash_bytes(lower.as_bytes())
}

pub(crate) fn invalid_edit_recovery_semantic_no_progress_key(
    envelope: &InvalidEditRecoveryEnvelope,
) -> String {
    let candidate_target = envelope
        .candidate_target
        .as_deref()
        .unwrap_or("missing_candidate_target");
    let parser_error_family = envelope
        .parser_error_family
        .as_deref()
        .unwrap_or("unknown_parser_family");
    let result_hash = envelope
        .result_hash
        .as_deref()
        .unwrap_or("missing_result_hash");
    let recovery_action = envelope
        .recovery_action
        .as_deref()
        .unwrap_or("missing_recovery_action");
    format!(
        "failed_edit_recovery|failure_kind={}|tool={}|parser_family={parser_error_family}|recovery_action={recovery_action}|result_hash={result_hash}|candidate_target={candidate_target}|targets={}|submitted={}|active_submitted={}|inactive_submitted={}",
        envelope.failure_kind,
        envelope.tool_name,
        envelope.active_targets.join(","),
        envelope.submitted_targets.join(","),
        envelope.active_submitted_targets.join(","),
        envelope.inactive_submitted_targets.join(","),
    )
}

pub(crate) fn should_terminalize_invalid_edit_arguments_no_progress(count: usize) -> bool {
    count >= INVALID_EDIT_ARGUMENTS_TERMINAL_THRESHOLD
}

pub(crate) fn invalid_edit_arguments_terminal_message(
    effective_tool_name: &str,
    count: usize,
    metadata: &Value,
) -> String {
    let active_targets = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("active_targets"))
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let feedback = metadata.get("tool_feedback_envelope");
    let parser_error_family = feedback
        .and_then(|feedback| feedback.get("parser_error_family"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let raw_argument_shape_hash = feedback
        .and_then(|feedback| feedback.get("raw_argument_shape_hash"))
        .and_then(Value::as_str)
        .unwrap_or("missing-raw-shape");
    format!(
        "Tool `{effective_tool_name}` returned invalid edit arguments with no filesystem side effects {count} time(s). Runtime stopped before repeating the same corrective ToolOutput until the outer timeout. parser_error_family={parser_error_family}; raw_argument_shape_hash={raw_argument_shape_hash}. Correct the edit call schema for active target(s): {active_targets}."
    )
}

pub(crate) fn is_invalid_tool_arguments_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("missing field")
        || lower.contains("invalid type")
        || lower.contains("unknown field")
        || lower.contains("expected")
        || lower.contains("context mismatch")
        || lower.contains("failed to find expected lines")
        || lower.contains("could not find expected lines")
        || lower.contains("tool patch error")
        || lower.contains("add file body line")
}

pub(crate) fn apply_patch_context_mismatch_enters_invalid_edit_lifecycle_fixture_passes() -> bool {
    is_invalid_tool_arguments_error("context mismatch while applying patch")
        && is_invalid_tool_arguments_error("Failed to find expected lines in component.py")
        && invalid_edit_parser_error_family("context mismatch while applying patch")
            == "apply_patch_context_mismatch"
        && invalid_edit_parser_error_family("Failed to find expected lines in component.py")
            == "apply_patch_context_mismatch"
}

pub(crate) fn repair_write_arguments_from_active_target(
    tool_name: &str,
    arguments_json: &str,
    active_targets: &[Utf8PathBuf],
) -> Option<String> {
    if tool_name != "write" {
        return None;
    }
    let target = singleton_relative_active_target(active_targets)?;
    let mut candidates = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(arguments_json) {
        candidates.push(value);
    } else if let Some(parse_error) = serde_json::from_str::<Value>(arguments_json)
        .err()
        .map(|error| error.to_string())
        && is_malformed_json_string_eof(&parse_error)
    {
        let trimmed = arguments_json.trim_end();
        let mut json_candidates = Vec::new();
        if let Some(prefix) = trimmed.strip_suffix('}') {
            json_candidates.push(format!("{prefix}\"}}"));
        }
        json_candidates.push(format!("{trimmed}\"}}"));
        candidates.extend(
            json_candidates
                .into_iter()
                .filter_map(|candidate| serde_json::from_str::<Value>(&candidate).ok()),
        );
    }

    candidates.into_iter().find_map(|mut value| {
        let object = value.as_object_mut()?;
        let content_present = object
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty());
        if !content_present {
            return None;
        }
        if object
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(content_contains_embedded_write_path_authority)
        {
            return None;
        }
        let path_missing = object
            .get("path")
            .and_then(Value::as_str)
            .is_none_or(|path| path.trim().is_empty());
        if !path_missing {
            return None;
        }
        object.insert("path".to_string(), Value::String(target.clone()));
        if !valid_write_arguments_value(&value) {
            return None;
        }
        normalize_repaired_write_arguments_value(&mut value);
        serde_json::to_string(&value).ok()
    })
}

#[derive(Clone, Debug)]
pub(crate) struct EscapedSourceWriteCandidate {
    target: Utf8PathBuf,
    original_arguments: Value,
    pub(crate) effective_arguments_json: String,
    payload_hash: String,
}

impl EscapedSourceWriteCandidate {
    pub(crate) fn into_candidate_repair_edit(
        self,
        source_call_id: crate::session::ToolCallId,
    ) -> CandidateRepairEdit {
        CandidateRepairEdit {
            candidate_id: CandidateRepairId::new(),
            proposal_id: ToolProposalId::new(),
            source_call_id,
            proposed_tool: ToolName::Write,
            target_path: Some(self.target),
            original_arguments: self.original_arguments,
            normalized_edit_intent:
                "admit escaped whole-file Python source candidate as real-newline source write"
                    .to_string(),
            semantic_class: "escaped_source_write_candidate_normalized".to_string(),
            validity: CandidateRepairValidity::Admitted,
            payload_hash: self.payload_hash,
            aligned_failure_refs: vec!["python_source_executable_content_shape".to_string()],
            evidence_refs: vec![
                "candidate_repair_edit".to_string(),
                "escaped_source_write_normalized".to_string(),
                "side_effects_deferred_until_normalized_write".to_string(),
            ],
        }
    }
}

pub(crate) fn normalized_escaped_source_write_candidate(
    tool_name: &str,
    arguments_json: &str,
    active_targets: &[Utf8PathBuf],
) -> Option<EscapedSourceWriteCandidate> {
    if tool_name != "write" {
        return None;
    }
    let mut value = serde_json::from_str::<Value>(arguments_json).ok()?;
    let object = value.as_object_mut()?;
    let target = object
        .get("path")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if target.is_empty()
        || !crate::agent::content_shape_contract::python_source_target_requires_executable_shape(
            &target,
        )
    {
        return None;
    }
    if !active_targets.is_empty()
        && !active_targets.iter().any(|active| {
            active_target_match_key(active.as_str()) == active_target_match_key(&target)
        })
    {
        return None;
    }
    let content = object.get("content").and_then(Value::as_str)?.to_string();
    if crate::agent::content_shape_contract::write_content_matches_required_target(
        &target, &content,
    )
        || !crate::agent::content_shape_contract::python_source_content_is_escaped_whole_file_string(
            &content,
        )
    {
        return None;
    }
    let normalized = normalize_escaped_whole_file_source_candidate(&target, &content)?;
    object.insert("content".to_string(), Value::String(normalized.clone()));
    let effective_arguments_json = serde_json::to_string(&value).ok()?;
    let payload_hash = crate::harness::artifact::hash_bytes(
        format!(
            "escaped_source_write_candidate:{}:{}",
            target,
            crate::harness::artifact::hash_bytes(normalized.as_bytes())
        )
        .as_bytes(),
    );
    Some(EscapedSourceWriteCandidate {
        target: Utf8PathBuf::from(target),
        original_arguments: serde_json::from_str::<Value>(arguments_json).ok()?,
        effective_arguments_json,
        payload_hash,
    })
}

fn normalize_escaped_whole_file_source_candidate(target: &str, content: &str) -> Option<String> {
    let normalized = content
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\'", "'");

    let mut candidates = Vec::new();
    if let Some(unwrapped) = strip_outer_source_string_literal_wrapper(&normalized) {
        candidates.push(unwrapped);
    }
    candidates.push(trim_unbalanced_trailing_triple_quote(normalized.clone()));
    candidates.push(normalized);

    candidates
        .into_iter()
        .map(|candidate| ensure_single_trailing_newline(candidate))
        .find(|candidate| {
            crate::agent::content_shape_contract::write_content_matches_required_target(
                target, candidate,
            )
        })
}

fn strip_outer_source_string_literal_wrapper(content: &str) -> Option<String> {
    let trimmed = content.trim();
    for marker in ["\"\"\"", "'''"] {
        if let Some(inner) = trimmed
            .strip_prefix(marker)
            .and_then(|value| value.strip_suffix(marker))
        {
            return Some(inner.trim_matches(['\r', '\n']).to_string());
        }
    }
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
    {
        return Some(inner.trim_matches(['\r', '\n']).to_string());
    }
    None
}

fn trim_unbalanced_trailing_triple_quote(mut content: String) -> String {
    let trimmed = content.trim_end();
    for marker in ["\"\"\"", "'''"] {
        if trimmed.ends_with(marker) && trimmed.matches(marker).count() % 2 == 1 {
            let new_len = trimmed.len().saturating_sub(marker.len());
            content.truncate(new_len);
            return ensure_single_trailing_newline(content.trim_end().to_string());
        }
    }
    content
}

fn ensure_single_trailing_newline(mut content: String) -> String {
    while content.ends_with(' ') || content.ends_with('\t') || content.ends_with('\r') {
        content.pop();
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content
}

fn active_target_match_key(path: &str) -> String {
    path.replace('\\', "/")
        .trim()
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

fn content_contains_embedded_write_path_authority(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("\",\"path\"")
        || lower.contains("\\\",\\\"path\\\"")
        || lower.contains("\\\"path\\\":")
        || lower.contains(",'path'")
}

fn singleton_relative_active_target(active_targets: &[Utf8PathBuf]) -> Option<String> {
    let target = active_targets.first()?;
    if active_targets.len() != 1 || target.is_absolute() {
        return None;
    }
    let normalized = target.as_str().replace('\\', "/");
    let trimmed = normalized.trim().trim_start_matches("./");
    if trimmed.is_empty()
        || trimmed.contains(':')
        || trimmed.starts_with('/')
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn repair_unambiguous_malformed_edit_arguments_json(
    tool_name: &str,
    arguments_json: &str,
) -> Option<String> {
    if tool_name != "write" || serde_json::from_str::<Value>(arguments_json).is_ok() {
        return None;
    }
    let parse_error = serde_json::from_str::<Value>(arguments_json)
        .err()
        .map(|error| error.to_string())?;
    if !is_malformed_json_string_eof(&parse_error) {
        return None;
    }
    let trimmed = arguments_json.trim_end();
    let mut candidates = Vec::new();
    if let Some(prefix) = trimmed.strip_suffix('}') {
        candidates.push(format!("{prefix}\"}}"));
    }
    candidates.push(format!("{trimmed}\"}}"));
    candidates.into_iter().find_map(|candidate| {
        let mut value = serde_json::from_str::<Value>(&candidate).ok()?;
        if !valid_write_arguments_value(&value) {
            return None;
        }
        normalize_repaired_write_arguments_value(&mut value);
        serde_json::to_string(&value).ok()
    })
}

pub(crate) fn valid_write_arguments_value(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty())
        && object
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty())
}

pub(crate) fn normalize_repaired_write_arguments_value(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let path_is_python = object
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".py"));
    if !path_is_python {
        return;
    }
    let Some(content) = object.get("content").and_then(Value::as_str) else {
        return;
    };
    let normalized = normalize_python_source_literal_newlines(content);
    if normalized != content {
        object.insert("content".to_string(), Value::String(normalized));
    }
}

fn normalize_python_source_literal_newlines(content: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum QuoteState {
        None,
        Single,
        Double,
        TripleSingle,
        TripleDouble,
    }

    let mut output = String::with_capacity(content.len());
    let mut state = QuoteState::None;
    let mut escape = false;
    let mut i = 0usize;
    while i < content.len() {
        let rest = &content[i..];
        if rest.starts_with("\\r\\n") {
            if matches!(
                state,
                QuoteState::None | QuoteState::TripleSingle | QuoteState::TripleDouble
            ) {
                output.push('\n');
            } else {
                output.push_str("\\r\\n");
            }
            i += 4;
            escape = false;
            continue;
        }
        if rest.starts_with("\\n") {
            if matches!(
                state,
                QuoteState::None | QuoteState::TripleSingle | QuoteState::TripleDouble
            ) {
                output.push('\n');
            } else {
                output.push_str("\\n");
            }
            i += 2;
            escape = false;
            continue;
        }
        if rest.starts_with("\"\"\"")
            && matches!(state, QuoteState::None | QuoteState::TripleDouble)
        {
            state = if state == QuoteState::TripleDouble {
                QuoteState::None
            } else {
                QuoteState::TripleDouble
            };
            output.push_str("\"\"\"");
            i += 3;
            escape = false;
            continue;
        }
        if rest.starts_with("'''") && matches!(state, QuoteState::None | QuoteState::TripleSingle) {
            state = if state == QuoteState::TripleSingle {
                QuoteState::None
            } else {
                QuoteState::TripleSingle
            };
            output.push_str("'''");
            i += 3;
            escape = false;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        match state {
            QuoteState::None if ch == '"' => state = QuoteState::Double,
            QuoteState::None if ch == '\'' => state = QuoteState::Single,
            QuoteState::Double if ch == '"' && !escape => state = QuoteState::None,
            QuoteState::Single if ch == '\'' && !escape => state = QuoteState::None,
            _ => {}
        }
        output.push(ch);
        escape = matches!(state, QuoteState::Single | QuoteState::Double) && ch == '\\' && !escape;
        if ch != '\\' {
            escape = false;
        }
        i += ch.len_utf8();
    }
    output
}

fn tool_choice_label(choice: &ToolChoice) -> &'static str {
    match choice {
        ToolChoice::Auto => "auto",
        ToolChoice::Required => "required",
        ToolChoice::None => "none",
        ToolChoice::Named(_) => "named",
    }
}

pub(crate) fn invalid_edit_recovery_uses_open_target_when_candidate_is_inactive_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("test_widget.py")],
        ..SessionStateSnapshot::default()
    };
    state.completion.closeout_ready = false;
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let arguments = json!({
        "patch_text": "*** Begin Patch\n*** Update File: widget.py\n+def add(a, b):\n+    return a + b\n*** End Patch\n*** Update File: test_widget.py\n*** Begin Patch\n*** Update File: test_widget.py\n+import unittest\n+import widget\n*** End Patch\n"
    })
    .to_string();
    let result = invalid_tool_arguments_result(
        "apply_patch",
        &arguments,
        "Tool patch error: Add File body line must start with +",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Required),
    );
    let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &allowed,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    envelope.candidate_target.as_deref() == Some("widget.py")
        && envelope
            .submitted_targets
            .contains(&"widget.py".to_string())
        && envelope
            .submitted_targets
            .contains(&"test_widget.py".to_string())
        && envelope
            .active_submitted_targets
            .contains(&"test_widget.py".to_string())
        && envelope
            .inactive_submitted_targets
            .contains(&"widget.py".to_string())
        && envelope.active_targets == vec!["test_widget.py".to_string()]
        && envelope
            .prompt
            .contains("Submitted patch declared active target(s) `test_widget.py`")
        && envelope
            .prompt
            .contains("inactive target(s) `widget.py`")
        && envelope
            .prompt
            .contains("resend a target-only edit")
        && !envelope
            .prompt
            .contains("It is not currently an open target")
        && envelope
            .prompt
            .contains("Required recovery operation: submit a corrected `apply_patch` content-changing edit for `test_widget.py`")
        && !envelope.prompt.contains("latest attempted open target")
        && envelope
            .prompt
            .contains("Active generated-test target contract")
        && envelope.prompt.contains("test_widget.py")
}

pub(crate) fn mixed_target_apply_patch_preserves_active_hunk_evidence_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        ..SessionStateSnapshot::default()
    };
    state.completion.open_work_count = 1;
    let allowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: calculator.py\n+def calculate(a, b):\n+    return a + b\n*** End Patch\n*** Add File: test_calculator.py\n+import unittest\n+import calculator\n+\n+class TestCalculator(unittest.TestCase):\n+    def test_add(self):\n+        self.assertEqual(calculator.calculate(2, 3), 5)\n*** End Patch"
    })
    .to_string();
    let result = invalid_tool_arguments_result(
        "apply_patch",
        &arguments,
        "tool patch error: unexpected patch line `*** End Patch`. Use the exact apply_patch grammar.",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Required),
    );
    let Some(feedback) = result.metadata.get("tool_feedback_envelope") else {
        return false;
    };
    let Some(envelope) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &allowed,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    let feedback_array_contains = |key: &str, expected: &str| {
        feedback
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|targets| {
                targets
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|value| value == expected)
            })
    };
    result
        .metadata
        .get("side_effects_applied")
        .and_then(Value::as_bool)
        == Some(false)
        && feedback
            .get("recovery_action")
            .and_then(Value::as_str)
            == Some("mixed_target_apply_patch_rewrite_target_only")
        && feedback_array_contains("active_submitted_targets", "test_calculator.py")
        && feedback_array_contains("inactive_submitted_targets", "calculator.py")
        && envelope.candidate_target.as_deref() == Some("calculator.py")
        && envelope
            .submitted_targets
            .contains(&"test_calculator.py".to_string())
        && envelope
            .active_submitted_targets
            .contains(&"test_calculator.py".to_string())
        && envelope
            .inactive_submitted_targets
            .contains(&"calculator.py".to_string())
        && envelope
            .prompt
            .contains("Submitted patch declared active target(s) `test_calculator.py`")
        && envelope
            .prompt
            .contains("do not include inactive source hunks")
        && envelope
            .prompt
            .contains("Required recovery operation: submit a corrected `apply_patch` content-changing edit for `test_calculator.py`")
}

#[cfg(test)]
mod tests {
    #[test]
    fn mixed_target_apply_patch_preserves_active_hunk_evidence() {
        assert!(super::mixed_target_apply_patch_preserves_active_hunk_evidence_fixture_passes());
    }

    #[test]
    fn invalid_edit_recovery_uses_open_target_when_candidate_is_inactive() {
        assert!(
            super::invalid_edit_recovery_uses_open_target_when_candidate_is_inactive_fixture_passes(
            )
        );
    }
}
