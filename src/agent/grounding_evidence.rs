use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;

use crate::agent::lifecycle_kernel::provider_replay_result_is_supporting_context;
use crate::agent::tool_orchestrator::{AuthoringGroundingRecoveryEnvelope, ToolLifecycleRuntime};
use crate::protocol::{
    HistoryItem, HistoryItemPayload, OperationIntent, ToolLifecycleStatus,
    canonical_tool_call_arguments,
};
use crate::session::{
    ContractStatus, DocsArea, DocsDeliverableCoverage, DocsGroundingRequirement,
    SessionStateSnapshot,
};
use crate::tool::ToolName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DocsContentGroundingClass {
    Repository,
    Tests,
}

pub(crate) fn docs_route_has_required_content_grounding_evidence(
    state: &SessionStateSnapshot,
    history_items: &[HistoryItem],
) -> bool {
    let required = docs_route_required_content_grounding_classes(state);
    let observed = docs_route_observed_content_grounding_classes(history_items);
    !observed.is_empty() && required.iter().all(|class| observed.contains(class))
}

fn docs_route_required_content_grounding_classes(
    state: &SessionStateSnapshot,
) -> BTreeSet<DocsContentGroundingClass> {
    let mut required = BTreeSet::from([DocsContentGroundingClass::Repository]);
    if let Some(coverage) = docs_route_active_deliverable_coverage(state) {
        let requires_tests_topic = coverage
            .required_topics
            .iter()
            .any(|topic| topic.eq_ignore_ascii_case("tests"));
        let requires_tests_area = coverage.required_areas.contains(&DocsArea::Tests);
        let requires_tests_grounding = coverage.grounding.iter().any(|grounding| {
            grounding.requirement == DocsGroundingRequirement::Tests
                && grounding.status == ContractStatus::Satisfied
        });
        if requires_tests_topic || requires_tests_area || requires_tests_grounding {
            required.insert(DocsContentGroundingClass::Tests);
        }
    }
    required
}

fn docs_route_active_deliverable_coverage(
    state: &SessionStateSnapshot,
) -> Option<&DocsDeliverableCoverage> {
    let docs = state.docs_route.as_ref()?;
    if let Some(active) = docs.active_deliverable.as_ref() {
        if let Some(coverage) = docs
            .deliverables
            .iter()
            .find(|coverage| coverage.target == *active)
        {
            return Some(coverage);
        }
    }
    docs.deliverables.first()
}

fn docs_route_observed_content_grounding_classes(
    history_items: &[HistoryItem],
) -> BTreeSet<DocsContentGroundingClass> {
    let mut tool_calls = BTreeMap::<String, (ToolName, Value)>::new();
    let mut observed = BTreeSet::new();
    for item in history_items {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let args =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments)
                        .clone();
                tool_calls.insert(call_id.to_string(), (*tool, args));
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                output_text,
                metadata,
                success,
                ..
            } => {
                if !matches!(status, ToolLifecycleStatus::Completed)
                    || success == &Some(false)
                    || !docs_route_tool_output_is_supporting_context(metadata, output_text)
                {
                    continue;
                }
                let Some((tool, arguments)) = tool_calls.get(&call_id.to_string()) else {
                    continue;
                };
                if docs_route_content_grounding_tool(*tool)
                    && docs_route_tool_output_has_content_bearing_repository_evidence(
                        *tool,
                        metadata,
                        output_text,
                    )
                {
                    observed.insert(DocsContentGroundingClass::Repository);
                    if docs_route_tool_output_has_test_content_evidence(
                        *tool,
                        arguments,
                        output_text,
                    ) {
                        observed.insert(DocsContentGroundingClass::Tests);
                    }
                }
            }
            _ => {}
        }
    }
    observed
}

fn docs_route_tool_output_has_test_content_evidence(
    tool: ToolName,
    arguments: &Value,
    output_text: &str,
) -> bool {
    if docs_route_argument_path_is_test_content(arguments) {
        return true;
    }
    let evidence_text = docs_route_tool_output_evidence_text(output_text);
    match tool {
        ToolName::Grep => evidence_text
            .lines()
            .filter_map(docs_route_grep_line_path)
            .any(path_looks_like_test_content),
        ToolName::Read | ToolName::DoclingConvert | ToolName::McpCall => {
            path_looks_like_test_content(evidence_text)
        }
        _ => false,
    }
}

fn docs_route_argument_path_is_test_content(arguments: &Value) -> bool {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(path_looks_like_test_content)
        || arguments
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(path_looks_like_test_content)
}

fn docs_route_grep_line_path(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(index) = lower.find(".py:") {
        return Some(trimmed[..index + 3].trim());
    }
    let (path, _) = trimmed.split_once(':')?;
    Some(path.trim())
}

fn docs_route_tool_output_is_supporting_context(metadata: &Value, output_text: &str) -> bool {
    ToolLifecycleRuntime::operation_progress_class_from_metadata(metadata)
        == Some("supporting_context")
        || provider_replay_result_is_supporting_context(output_text)
}

fn docs_route_tool_output_has_content_bearing_repository_evidence(
    tool: ToolName,
    metadata: &Value,
    output_text: &str,
) -> bool {
    if metadata_bool(metadata, "corrective_result") == Some(true)
        || metadata_string(metadata, "blocked_reason").is_some()
    {
        return false;
    }
    let evidence_text = docs_route_tool_output_evidence_text(output_text);
    if evidence_text.trim().is_empty() {
        return false;
    }
    match tool {
        ToolName::Grep => {
            if metadata_usize(metadata, "total_matches") == Some(0) {
                return false;
            }
            metadata_usize(metadata, "total_matches").is_some_and(|matches| matches > 0)
                || docs_route_grep_output_has_match_line(evidence_text)
        }
        ToolName::Read | ToolName::DoclingConvert | ToolName::McpCall => true,
        _ => false,
    }
}

fn docs_route_tool_output_evidence_text(output_text: &str) -> &str {
    output_text
        .split("\n\n[tool feedback]")
        .next()
        .unwrap_or(output_text)
}

fn docs_route_grep_output_has_match_line(output_text: &str) -> bool {
    output_text.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && line.matches(':').count() >= 2
    })
}

fn metadata_usize(metadata: &Value, key: &str) -> Option<usize> {
    metadata
        .get(key)
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get(key))
        })
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn metadata_bool(metadata: &Value, key: &str) -> Option<bool> {
    metadata
        .get(key)
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get(key))
        })
        .and_then(Value::as_bool)
}

fn metadata_string<'a>(metadata: &'a Value, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get(key))
        })
        .and_then(Value::as_str)
}

fn docs_route_content_grounding_tool(tool: ToolName) -> bool {
    matches!(
        tool,
        ToolName::Read | ToolName::Grep | ToolName::DoclingConvert | ToolName::McpCall
    )
}

pub(crate) fn active_authoring_targets_need_grounding(
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
    turn_grounded_targets: &BTreeSet<String>,
) -> bool {
    !authoring_missing_grounding_targets(
        history_items,
        state,
        workspace_root,
        turn_grounded_targets,
    )
    .is_empty()
}

pub(crate) fn authoring_grounding_recovery_envelope(
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
    turn_grounded_targets: &BTreeSet<String>,
) -> AuthoringGroundingRecoveryEnvelope {
    let active_targets = active_authoring_target_keys(state);
    let missing = authoring_missing_grounding_targets(
        history_items,
        state,
        workspace_root,
        turn_grounded_targets,
    );
    let existing_targets = active_targets
        .iter()
        .filter(|target| workspace_root.join(target.as_str()).exists())
        .cloned()
        .collect::<BTreeSet<_>>();
    let consumed_targets = existing_targets
        .iter()
        .filter(|target| !missing.contains(*target))
        .cloned()
        .collect::<Vec<_>>();
    AuthoringGroundingRecoveryEnvelope {
        active_targets: active_targets.into_iter().collect(),
        consumed_targets,
        missing_grounding_targets: missing.into_iter().collect(),
    }
}

pub(crate) fn authoring_grounding_recovery_obligation(
    envelope: &AuthoringGroundingRecoveryEnvelope,
) -> crate::protocol::TurnObligation {
    crate::protocol::TurnObligation {
        obligation_id: "authoring_target_grounding_recovery".to_string(),
        kind: crate::protocol::ObligationKind::Repair,
        summary: format!(
            "Authoring grounding recovery must distinguish consumed active targets from remaining read targets. Consumed targets: {}. Remaining read targets: {}.",
            envelope.consumed_text(),
            envelope.missing_text()
        ),
        targets: envelope
            .active_targets
            .iter()
            .map(Utf8PathBuf::from)
            .collect(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: Vec::new(),
        verification_commands: Vec::new(),
        contract_refs: vec!["authoring_target_grounding_recovery".to_string()],
        evidence_refs: vec![crate::protocol::EvidenceRef {
            source: "authoring_target_grounding".to_string(),
            reference: envelope.evidence_ref(),
        }],
        status: crate::protocol::ObligationStatus::Open,
    }
}

pub(crate) fn authoring_missing_grounding_targets(
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
    turn_grounded_targets: &BTreeSet<String>,
) -> BTreeSet<String> {
    let active_targets = active_authoring_target_keys(state);
    if active_targets.is_empty() {
        return BTreeSet::new();
    }
    let existing_targets = active_targets
        .iter()
        .filter(|target| workspace_root.join(target.as_str()).exists())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut latest_change = BTreeMap::<String, i64>::new();
    let mut read_calls = BTreeMap::<String, String>::new();
    let mut latest_read = BTreeMap::<String, i64>::new();

    for item in history_items {
        let order = history_item_order_for_grounding(item);
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                    else {
                        continue;
                    };
                    let changed = normalize_path_for_target_match(path.as_str());
                    if let Some(target) = matching_active_target_key(&changed, &active_targets) {
                        latest_change
                            .entry(target)
                            .and_modify(|existing| *existing = (*existing).max(order))
                            .or_insert(order);
                    }
                }
            }
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Read => {
                let tool_arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                if let Some(path) = tool_arguments.get("path").and_then(Value::as_str) {
                    let read_path = normalize_path_for_target_match(path);
                    if let Some(target) = matching_active_target_key(&read_path, &active_targets) {
                        read_calls.insert(call_id.to_string(), target);
                    }
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                ..
            } if matches!(status, ToolLifecycleStatus::Completed) && success != &Some(false) => {
                if let Some(target) = read_calls.get(&call_id.to_string()) {
                    latest_read
                        .entry(target.clone())
                        .and_modify(|existing| *existing = (*existing).max(order))
                        .or_insert(order);
                }
            }
            _ => {}
        }
    }

    existing_targets
        .into_iter()
        .filter(|target| {
            if turn_grounded_targets.contains(target) {
                return false;
            }
            if !latest_read.contains_key(target) {
                return true;
            }
            latest_change.get(target).is_some_and(|change_order| {
                latest_read
                    .get(target)
                    .is_none_or(|read_order| *read_order < *change_order)
            })
        })
        .collect()
}

pub(crate) fn active_authoring_target_keys(state: &SessionStateSnapshot) -> BTreeSet<String> {
    state
        .active_targets
        .iter()
        .map(|target| normalize_path_for_target_match(target.as_str()))
        .collect::<BTreeSet<_>>()
}

pub(crate) fn record_authoring_grounded_active_target(
    grounded_targets: &mut BTreeSet<String>,
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
) {
    if effective_tool_name != "read"
        || ToolLifecycleRuntime::operation_progress_class_from_metadata(metadata)
            != Some("supporting_context")
        || !ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
            metadata, state,
        )
    {
        return;
    }
    let Some(path) = metadata.get("path").and_then(Value::as_str) else {
        return;
    };
    let active_targets = active_authoring_target_keys(state);
    if let Some(target) =
        matching_active_target_key(&normalize_path_for_target_match(path), &active_targets)
    {
        grounded_targets.insert(target);
    }
}

pub(crate) fn matching_active_target_key(
    path: &str,
    active_targets: &BTreeSet<String>,
) -> Option<String> {
    active_targets.iter().find_map(|target| {
        if path == target || path.ends_with(&format!("/{target}")) {
            Some(target.clone())
        } else {
            None
        }
    })
}

pub(crate) fn history_has_unread_source_change_for_generated_test(
    history_items: &[HistoryItem],
) -> bool {
    let mut latest_source_change = BTreeMap::<String, i64>::new();
    let mut read_calls = BTreeMap::<String, String>::new();
    let mut latest_source_read = BTreeMap::<String, i64>::new();

    for item in history_items {
        let order = history_item_order_for_grounding(item);
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                    else {
                        continue;
                    };
                    let normalized = normalize_path_for_target_match(path.as_str());
                    if source_reference_target_for_generated_test(&normalized) {
                        latest_source_change
                            .entry(normalized)
                            .and_modify(|existing| *existing = (*existing).max(order))
                            .or_insert(order);
                    }
                }
            }
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Read => {
                let tool_arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                if let Some(path) = tool_arguments.get("path").and_then(Value::as_str) {
                    read_calls.insert(call_id.to_string(), normalize_path_for_target_match(path));
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                ..
            } if matches!(status, ToolLifecycleStatus::Completed) && success != &Some(false) => {
                if let Some(path) = read_calls.get(&call_id.to_string())
                    && source_reference_target_for_generated_test(path)
                {
                    latest_source_read
                        .entry(path.clone())
                        .and_modify(|existing| *existing = (*existing).max(order))
                        .or_insert(order);
                }
            }
            _ => {}
        }
    }

    latest_source_change
        .into_iter()
        .any(|(path, change_order)| {
            latest_source_read
                .get(&path)
                .is_none_or(|read_order| *read_order < change_order)
        })
}

pub(crate) fn history_has_current_source_reference_read_for_generated_test(
    history_items: &[HistoryItem],
) -> bool {
    let mut latest_source_change = BTreeMap::<String, i64>::new();
    let mut read_calls = BTreeMap::<String, String>::new();
    let mut latest_source_read = BTreeMap::<String, i64>::new();

    for item in history_items {
        let order = history_item_order_for_grounding(item);
        match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => {
                for change in changes {
                    let Some(path) = change.path_after.as_ref().or(change.path_before.as_ref())
                    else {
                        continue;
                    };
                    let normalized = normalize_path_for_target_match(path.as_str());
                    if source_reference_target_for_generated_test(&normalized) {
                        latest_source_change
                            .entry(normalized)
                            .and_modify(|existing| *existing = (*existing).max(order))
                            .or_insert(order);
                    }
                }
            }
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Read => {
                let tool_arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                if let Some(path) = tool_arguments.get("path").and_then(Value::as_str) {
                    read_calls.insert(call_id.to_string(), normalize_path_for_target_match(path));
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                ..
            } if matches!(status, ToolLifecycleStatus::Completed) && success != &Some(false) => {
                if let Some(path) = read_calls.get(&call_id.to_string())
                    && source_reference_target_for_generated_test(path)
                {
                    latest_source_read
                        .entry(path.clone())
                        .and_modify(|existing| *existing = (*existing).max(order))
                        .or_insert(order);
                }
            }
            _ => {}
        }
    }

    latest_source_change
        .into_iter()
        .any(|(path, change_order)| {
            latest_source_read
                .get(&path)
                .is_some_and(|read_order| *read_order >= change_order)
        })
}

fn source_reference_target_for_generated_test(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.ends_with(".py")
        && !lower.contains("/__pycache__/")
        && !path_looks_like_test_content(&lower)
}

fn history_item_order_for_grounding(item: &HistoryItem) -> i64 {
    if item.sequence_no != 0 {
        return item.sequence_no;
    }
    item.created_at_ms
}

pub(crate) fn singleton_active_target_exists(
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
) -> bool {
    let Some(target) = state.active_targets.first() else {
        return false;
    };
    workspace_root.join(target.as_str()).exists()
}

pub(crate) fn generated_test_reference_consumed_read_requires_active_target(
    effective_tool_name: &str,
    arguments: &Value,
    state: &SessionStateSnapshot,
) -> bool {
    effective_tool_name == "read" && !metadata_path_matches_active_target(arguments, state)
}

pub(crate) fn metadata_path_matches_active_target(
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> bool {
    let Some(path) = metadata.get("path").and_then(Value::as_str) else {
        return false;
    };
    let normalized_path = normalize_path_for_target_match(path);
    state.active_targets.iter().any(|target| {
        let normalized_target = normalize_path_for_target_match(target.as_str());
        normalized_path == normalized_target
            || normalized_path.ends_with(&format!("/{normalized_target}"))
    })
}

pub(crate) fn normalize_path_for_target_match(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn path_looks_like_test_content(value: &str) -> bool {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    normalized.rsplit('/').next().is_some_and(|file_name| {
        file_name.starts_with("test_")
            || file_name.ends_with("_test.py")
            || file_name.ends_with(".test.py")
    }) || normalized.contains("/tests/")
}
