use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};

use crate::agent::state::ActiveWorkContract;
use crate::protocol::{
    EvidenceRef, HistoryItem, HistoryItemId, HistoryItemPayload, ObligationKind, ObligationStatus,
    OperationIntent, ProjectionBundle, ProjectionId, ToolLifecycleStatus, ToolProgressEffect,
    TurnId, TurnObligation,
};
use crate::session::{MessageRole, SessionId, SessionStateSnapshot, TaskRoute, ToolCallId};
use crate::tool::ToolResult;

pub(crate) const DOCS_SPEC_SEMANTIC_RECONCILIATION_TERMINAL_THRESHOLD: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentationSemanticClaim {
    id: &'static str,
    description: String,
    evidence_refs: Vec<String>,
    subject_term_groups: Vec<Vec<String>>,
    predicate_term_groups: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DocumentationSemanticClaimContract {
    required_claims: Vec<DocumentationSemanticClaim>,
    prohibited_claims: Vec<DocumentationSemanticClaim>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DocumentationSemanticReport {
    satisfied_required_claims: Vec<SemanticClaimProjection>,
    missing_required_claims: Vec<SemanticClaimProjection>,
    prohibited_claims_present: Vec<SemanticClaimProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticClaimProjection {
    id: &'static str,
    description: String,
    evidence_refs: Vec<String>,
    observed_refs: Vec<String>,
    repair_snippets: Vec<String>,
}

impl DocumentationSemanticReport {
    fn is_clean(&self) -> bool {
        self.missing_required_claims.is_empty() && self.prohibited_claims_present.is_empty()
    }
}

pub(crate) fn latest_user_authority_text(history_items: &[HistoryItem]) -> Option<String> {
    let mut ordered_items = history_items.iter().collect::<Vec<_>>();
    ordered_items.sort_by(|left, right| {
        left.sequence_no
            .cmp(&right.sequence_no)
            .then_with(|| left.created_at_ms.cmp(&right.created_at_ms))
    });
    ordered_items
        .into_iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::UserTurn { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|part| match part {
                        crate::protocol::ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            HistoryItemPayload::Message {
                role: MessageRole::User,
                content,
                ..
            } => Some(content_parts_text(content)),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .last()
}

fn content_parts_text(content: &[crate::protocol::ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            crate::protocol::ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn docs_spec_semantic_reconciliation_result(
    tool_name: &str,
    arguments: &Value,
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
    workspace_root: &Utf8Path,
    latest_user_text: Option<&str>,
) -> Option<ToolResult> {
    if !matches!(tool_name, "write" | "apply_patch") {
        return None;
    }
    if !documentation_semantic_contract_applies(state, active_work, latest_user_text) {
        return None;
    }
    let contract = documentation_semantic_contract_from_authority(latest_user_text?)?;
    let candidate = documentation_candidate_from_tool(tool_name, arguments, workspace_root)?;
    if !candidate
        .targets
        .iter()
        .any(|target| documentation_target(Utf8Path::new(target)))
    {
        return None;
    }
    let report = reconcile_documentation_semantics(&contract, &candidate.content);
    if report.is_clean() {
        return None;
    }

    Some(docs_spec_semantic_reconciliation_tool_result(
        tool_name, arguments, candidate, report,
    ))
}

pub(crate) fn docs_spec_semantic_reconciliation_key(result: &ToolResult) -> String {
    let target_key = result
        .metadata
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let missing = result
        .metadata
        .get("missing_required_claims")
        .and_then(Value::as_array)
        .map(|claims| {
            claims
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let prohibited = result
        .metadata
        .get("prohibited_claims_present")
        .and_then(Value::as_array)
        .map(|claims| {
            claims
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    format!("docs_spec_semantic_reconciliation:{target_key}:{missing}:{prohibited}")
}

pub(crate) fn docs_spec_semantic_reconciliation_terminal_message(
    result: &ToolResult,
    correction_count: usize,
) -> String {
    let targets = result
        .metadata
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "documentation target".to_string());
    format!(
        "Docs/spec semantic reconciliation rejected contradictory documentation {correction_count} time(s). Runtime stopped before accepting artifact progress that violates the latest request authority. Targets: {targets}."
    )
}

pub(crate) fn docs_spec_semantic_reconciliation_recovery_obligation(
    history_items: &[HistoryItem],
    active_work: Option<&ActiveWorkContract>,
    workspace_root: &Utf8Path,
) -> Option<TurnObligation> {
    let active_targets = active_work
        .map(ActiveWorkContract::targets)
        .unwrap_or_default();
    if active_targets.is_empty() {
        return None;
    }
    let active_keys = target_keys(&active_targets, workspace_root);
    if active_keys.is_empty() {
        return None;
    }

    for (index, item) in history_items.iter().enumerate().rev() {
        let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload else {
            continue;
        };
        if metadata.get("docs_spec_semantic_reconciliation") != Some(&Value::Bool(true))
            || operation_progress_class(metadata)
                != Some("docs_spec_semantic_reconciliation_failed")
        {
            continue;
        }
        let targets = metadata_targets(metadata);
        let target_key_set = target_keys(&targets, workspace_root);
        if target_key_set.is_empty() || active_keys.is_disjoint(&target_key_set) {
            continue;
        }
        if later_file_change_touched_targets(&history_items[index + 1..], &target_key_set) {
            return None;
        }
        let missing_details = claim_details(metadata, "missing_required_claim_details");
        let prohibited_details = claim_details(metadata, "prohibited_claim_details");
        if missing_details.is_empty() && prohibited_details.is_empty() {
            return None;
        }
        let mut contract_refs = vec!["docs_spec_semantic_reconciliation_recovery".to_string()];
        contract_refs.extend(claim_ids(&missing_details));
        contract_refs.extend(claim_ids(&prohibited_details));
        contract_refs.sort();
        contract_refs.dedup();

        let mut evidence_refs = Vec::new();
        evidence_refs.extend(claim_evidence_refs(
            "missing_required_claim",
            &missing_details,
        ));
        evidence_refs.extend(claim_evidence_refs(
            "prohibited_claim_present",
            &prohibited_details,
        ));
        evidence_refs.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.reference.cmp(&right.reference))
        });
        evidence_refs.dedup_by(|left, right| {
            left.source == right.source && left.reference == right.reference
        });

        return Some(TurnObligation {
            obligation_id: "docs_semantic_reconciliation_recovery".to_string(),
            kind: ObligationKind::Repair,
            summary: format!(
                "Docs semantic reconciliation recovery must preserve claim-specific correction before rewriting {}. Missing required claims: {}. Prohibited claims present: {}.",
                target_display(&targets),
                claim_summary(&missing_details),
                claim_summary(&prohibited_details)
            ),
            targets,
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: Vec::new(),
            verification_commands: Vec::new(),
            contract_refs,
            evidence_refs,
            status: ObligationStatus::Open,
        });
    }
    None
}

fn operation_progress_class(metadata: &Value) -> Option<&str> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
}

fn metadata_targets(metadata: &Value) -> Vec<Utf8PathBuf> {
    metadata
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .filter(|target| !target.trim().is_empty())
                .map(Utf8PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn target_keys(targets: &[Utf8PathBuf], workspace_root: &Utf8Path) -> BTreeSet<String> {
    targets
        .iter()
        .filter_map(|target| target_key(target.as_str(), workspace_root))
        .collect()
}

fn target_key(target: &str, workspace_root: &Utf8Path) -> Option<String> {
    let relative = crate::workspace::project::workspace_relative_key_for_match(
        target,
        workspace_root.as_str(),
    );
    relative
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let key = crate::workspace::project::path_key_for_workspace_match(target);
            (!key.trim().is_empty()).then_some(key)
        })
        .map(|value| value.replace('\\', "/").to_ascii_lowercase())
}

fn later_file_change_touched_targets(
    history_items: &[HistoryItem],
    target_keys: &BTreeSet<String>,
) -> bool {
    history_items.iter().any(|item| {
        let HistoryItemPayload::FileChange { changes, .. } = &item.payload else {
            return false;
        };
        changes.iter().any(|change| {
            change
                .path_after
                .as_ref()
                .or(change.path_before.as_ref())
                .and_then(|target| target_key(target.as_str(), Utf8Path::new("")))
                .is_some_and(|key| target_keys.contains(&key))
        })
    })
}

fn claim_details(metadata: &Value, key: &str) -> Vec<Value> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|values| values.to_vec())
        .unwrap_or_default()
}

fn claim_ids(details: &[Value]) -> Vec<String> {
    details
        .iter()
        .filter_map(|detail| detail.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn claim_evidence_refs(kind: &str, details: &[Value]) -> Vec<EvidenceRef> {
    details
        .iter()
        .filter_map(|detail| {
            let id = detail.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let description = detail
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("claim detail unavailable");
            let evidence = detail_values_text(detail, "evidence_refs");
            let observed = detail_values_text(detail, "observed_refs");
            let repair = detail_values_text(detail, "repair_snippets");
            Some(EvidenceRef {
                source: "docs_semantic_reconciliation".to_string(),
                reference: format!(
                    "{kind}:{id}; description={description}; evidence={evidence}; observed={observed}; repair={repair}"
                ),
            })
        })
        .collect()
}

fn detail_values_text(detail: &Value, key: &str) -> String {
    let values = detail
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    if values.trim().is_empty() {
        "none".to_string()
    } else {
        values
    }
}

fn claim_summary(details: &[Value]) -> String {
    let ids = claim_ids(details);
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

fn target_display(targets: &[Utf8PathBuf]) -> String {
    if targets.is_empty() {
        "documentation target".to_string()
    } else {
        targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn docs_spec_semantic_reconciliation_fixture_passes() -> bool {
    let authority = "Update only docs/spec. The CLI grammar treats unknown two-token invocations such as `tool beta` as usage error exit code 1. Do not document or test those as unsupported function exit code 2.";
    let exact_authority = "Docs only. Incomplete binary-looking `python tool.py 8 +` must be usage error exit code 1. Unknown two-token `python tool.py log 10` must be usage error exit code 1; do not create unsupported-function exit code 2 tests.";
    let bad_doc = r#"
# Tool CLI

Unknown two-token invocations such as `tool beta` are usage error cases and exit code 1.

| Case | Exit |
| --- | --- |
| unknown two-token unsupported function | exit code 2 |
"#;
    let localized_bad_doc = r#"
# ツール CLI

未知の 2 token 入力 such as `tool beta` は usage error として exit code 1 を返す。

| 状況 | 処理内容 |
| --- | --- |
| 未知の 2 token 未定義の関数 | `sys.exit(2)` |
"#;
    let fixed_doc = r#"
# Tool CLI

Unknown two-token invocations such as `tool beta` are usage error cases and exit code 1.
Unsupported helper functions are internal API errors and are not a CLI fallback for unknown two-token input.
"#;
    let allowed_separate_unsupported_function_doc = r#"
# Tool CLI

Unknown two-token invocations such as `tool beta` are usage error cases and exit code 1.

| Case | Exit |
| --- | --- |
| unsupported helper function | exit code 2 |
"#;
    let too_generic_doc = r#"
# Tool CLI

Inputs with fewer than 3 tokens are usage error cases and exit code 1.
"#;
    let exact_fixed_doc = r#"
# Tool CLI

Incomplete binary CLI input such as `python tool.py 8 +` is a usage error and exits with exit code 1.
Unknown 2 トークン CLI input such as `python tool.py log 10` is a usage error and exits with exit code 1.
"#;
    let exact_table_fixed_doc = r#"
# Tool CLI

| command | behavior | exit |
| --- | --- | --- |
| `python tool.py 8 +` | usage error: show valid input format | exit code 1 |
| `python tool.py log 10` | usage error: show valid input format | exit code 1 |
"#;
    let negative_guidance_fixed_doc = r#"
# Tool CLI

| command | behavior | exit |
| --- | --- | --- |
| `python tool.py 8 +` | invalid incomplete input: show valid input format | exit code 1 |
| `python tool.py log 10` | unknown two-token input usage error: show valid input format | exit code 1 |

- Do not create generated tests that expect unknown two-token input to be an undefined function with exit code 2.
- 未知の2トークン入力 (`log 10` など) について、exit code 2 を期待する生成 test は作成しない。
"#;
    let Some(contract) = documentation_semantic_contract_from_authority(authority) else {
        return false;
    };
    let Some(exact_contract) = documentation_semantic_contract_from_authority(exact_authority)
    else {
        return false;
    };
    let bad = reconcile_documentation_semantics(&contract, bad_doc);
    let localized_bad = reconcile_documentation_semantics(&contract, localized_bad_doc);
    let fixed = reconcile_documentation_semantics(&contract, fixed_doc);
    let allowed_separate_unsupported_function =
        reconcile_documentation_semantics(&contract, allowed_separate_unsupported_function_doc);
    let too_generic = reconcile_documentation_semantics(&exact_contract, too_generic_doc);
    let exact_fixed = reconcile_documentation_semantics(&exact_contract, exact_fixed_doc);
    let exact_table_fixed =
        reconcile_documentation_semantics(&exact_contract, exact_table_fixed_doc);
    let negative_guidance_fixed =
        reconcile_documentation_semantics(&exact_contract, negative_guidance_fixed_doc);

    !bad.is_clean()
        && bad
            .prohibited_claims_present
            .iter()
            .any(|claim| claim.id == "unknown_two_token_cli_as_unsupported_function_exit_2")
        && !localized_bad.is_clean()
        && localized_bad
            .prohibited_claims_present
            .iter()
            .any(|claim| claim.id == "unknown_two_token_cli_as_undefined_function_exit_2")
        && fixed.is_clean()
        && allowed_separate_unsupported_function.is_clean()
        && !too_generic.is_clean()
        && too_generic.missing_required_claims.iter().any(|claim| {
            claim.id == "unknown_two_token_cli_usage_error_exit_1"
                && claim
                    .evidence_refs
                    .iter()
                    .any(|evidence| evidence.contains("python tool.py log 10"))
        })
        && exact_fixed.is_clean()
        && exact_table_fixed.is_clean()
        && negative_guidance_fixed.is_clean()
        && exact_table_fixed
            .satisfied_required_claims
            .iter()
            .any(|claim| {
                claim.id == "unknown_two_token_cli_usage_error_exit_1"
                    && claim
                        .observed_refs
                        .iter()
                        .any(|observed| observed.contains("python tool.py log 10"))
            })
        && exact_table_fixed
            .satisfied_required_claims
            .iter()
            .any(|claim| {
                claim.id == "incomplete_binary_cli_usage_error_exit_1"
                    && claim
                        .observed_refs
                        .iter()
                        .any(|observed| observed.contains("python tool.py 8 +"))
            })
        && negative_guidance_fixed
            .satisfied_required_claims
            .iter()
            .any(|claim| {
                claim.id == "incomplete_binary_cli_usage_error_exit_1"
                    && claim
                        .observed_refs
                        .iter()
                        .any(|observed| observed.contains("invalid incomplete input"))
            })
}

pub fn docs_spec_semantic_reconciliation_feedback_projection_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let target = Utf8PathBuf::from("docs/component-design.md");
    let history_items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::ToolOutput {
            call_id: ToolCallId::new(),
            status: ToolLifecycleStatus::Completed,
            title: "Docs/spec semantic reconciliation failed".to_string(),
            output_text: "Runtime rejected `write` before filesystem side effects.".to_string(),
            metadata: json!({
                "docs_spec_semantic_reconciliation": true,
                "operation_progress_class": "docs_spec_semantic_reconciliation_failed",
                "progress_effect": "no_progress",
                "targets": ["docs/component-design.md"],
                "missing_required_claim_details": [{
                    "id": "unknown_two_token_cli_usage_error_exit_1",
                    "description": "Document that unknown two-token CLI input is a usage error with exit code 1.",
                    "evidence_refs": ["python tool.py log 10"],
                    "observed_refs": [],
                    "repair_snippets": ["Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1."]
                }],
                "prohibited_claim_details": []
            }),
            success: Some(false),
            progress_effect: ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("docs-semantic-fixture".to_string()),
            verification_run: None,
        },
    }];
    let active_work = ActiveWorkContract::DocsRepair {
        deliverable: Some(target.clone()),
        pending_deliverables: Vec::new(),
        pending_summary: "rewrite docs/component-design.md".to_string(),
        route_contract_satisfied: false,
    };
    let Some(obligation) = docs_spec_semantic_reconciliation_recovery_obligation(
        &history_items,
        Some(&active_work),
        Utf8Path::new("C:/workspace/project"),
    ) else {
        return false;
    };
    let authority = crate::protocol::ActionAuthority {
        projection_id: ProjectionId::new(),
        required_action: Some(crate::protocol::RequiredAction {
            kind: crate::protocol::RequiredActionKind::EditTarget,
            tool: crate::tool::ToolName::Write,
            target: Some(target),
            command: None,
            projection_text: "write:docs/component-design.md".to_string(),
        }),
        required_verification_commands: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        allowed_tools: vec![crate::tool::ToolName::Write],
        forbidden_tools: Vec::new(),
        tool_choice: crate::protocol::ToolChoice::Named(crate::tool::ToolName::Write),
    };
    let obligations = crate::protocol::ObligationSet::new(vec![obligation.clone()]);
    let projection = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);
    let prompt = projection.prompt.render_prompt_block();
    let diagnostics = projection
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = projection
        .tool_result_feedback
        .render_control_projection()
        .text;
    obligation.evidence_refs.iter().any(|reference| {
        reference
            .reference
            .contains("unknown_two_token_cli_usage_error_exit_1")
            && reference.reference.contains("python tool.py log 10")
            && reference.reference.contains("exit code 1")
            && reference
                .reference
                .contains("repair=Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1.")
    }) && prompt.contains("docs_semantic_reconciliation")
        && prompt.contains("unknown_two_token_cli_usage_error_exit_1")
        && prompt.contains("python tool.py log 10")
        && prompt.contains("repair=Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1.")
        && diagnostics.contains("unknown_two_token_cli_usage_error_exit_1")
        && diagnostics.contains("repair=Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1.")
        && feedback.contains("python tool.py log 10")
        && feedback.contains("repair=Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1.")
}

pub(crate) fn latest_user_authority_text_uses_sequence_order_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 20,
            created_at_ms: 20,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::User,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "Latest docs authority: unknown two-token `python tool.py log 10` must be usage error exit code 1.".to_string(),
                }],
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 10,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![crate::protocol::ContentPart::Text {
                    text: "Older docs authority: unsupported function exit code 2.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    latest_user_authority_text(&items).is_some_and(|text| {
        text.contains("python tool.py log 10") && !text.contains("unsupported function exit code 2")
    })
}

pub fn docs_spec_semantic_reconciliation_tool_fixture_passes() -> bool {
    let authority = "Docs only: unknown two-token CLI input is a usage error with exit code 1; do not add unsupported-function exit-code-2 generated tests for that input.";
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.active_targets = vec![Utf8PathBuf::from("docs/spec.md")];
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("docs/spec.md")],
        verification_commands: vec![],
    };
    let result = docs_spec_semantic_reconciliation_result(
        "write",
        &json!({
            "path": "docs/spec.md",
            "content": "Unknown two-token CLI input is usage error exit code 1.\nUnknown two-token CLI input is treated as unsupported function exit code 2."
        }),
        &state,
        Some(&active_work),
        Utf8Path::new("C:/workspace"),
        Some(authority),
    );
    let result_ok = result.as_ref().is_some_and(|tool_result| {
        tool_result.recorded_changes.is_empty()
            && tool_result.change_summaries.is_empty()
            && tool_result
                .metadata
                .pointer("/tool_feedback_envelope/side_effects_applied")
                .and_then(Value::as_bool)
                == Some(false)
            && tool_result
                .metadata
                .get("prohibited_claims_present")
                .and_then(Value::as_array)
                .is_some_and(|claims| {
                    claims.iter().any(|claim| {
                        claim.as_str()
                            == Some("unknown_two_token_cli_as_unsupported_function_exit_2")
                    })
                })
            && tool_result
                .metadata
                .get("prohibited_claim_details")
                .and_then(Value::as_array)
                .is_some_and(|claims| {
                    claims.iter().any(|claim| {
                        claim
                            .get("observed_refs")
                            .and_then(Value::as_array)
                            .is_some_and(|refs| {
                                refs.iter().any(|value| {
                                    value.as_str().is_some_and(|text| {
                                        text.contains("unsupported function exit code 2")
                                    })
                                })
                            })
                    })
                })
    });
    let localized_result = docs_spec_semantic_reconciliation_result(
        "write",
        &json!({
            "path": "docs/spec.md",
            "content": "未知の 2 token CLI input は usage error として sys.exit(1) を返す。\n| 未知の 2 token 未定義の関数 | sys.exit(2) |"
        }),
        &state,
        Some(&active_work),
        Utf8Path::new("C:/workspace"),
        Some(authority),
    );
    let negative_guidance_result = docs_spec_semantic_reconciliation_result(
        "write",
        &json!({
            "path": "docs/spec.md",
            "content": "Incomplete binary input `python tool.py 8 +` is invalid input and exits with exit code 1.\nUnknown two-token `python tool.py log 10` is a usage error and exits with exit code 1.\nDo not create generated tests that expect unknown two-token input as undefined function exit code 2.\n未知の2トークン入力 (`log 10` など) について、exit code 2 を期待する生成 test は作成しない。"
        }),
        &state,
        Some(&active_work),
        Utf8Path::new("C:/workspace"),
        Some(
            "Docs only. Incomplete binary-looking `python tool.py 8 +` must be usage error exit code 1. Unknown two-token `python tool.py log 10` must be usage error exit code 1; do not create unsupported-function exit code 2 tests.",
        ),
    );
    let command_subject_result = docs_spec_semantic_reconciliation_result(
        "write",
        &json!({
            "path": "docs/spec.md",
            "content": "| Command | Classification | Exit code |\n| --- | --- | --- |\n| `python tool.py log 10` | usage error | exit code 1 |\n| `python tool.py 8 +` | usage error | exit code 1 |"
        }),
        &state,
        Some(&active_work),
        Utf8Path::new("C:/workspace"),
        Some(
            "Docs only. Incomplete binary-looking `python tool.py 8 +` must be usage error exit code 1. Unknown two-token `python tool.py log 10` must be usage error exit code 1.",
        ),
    );
    let localized_numeric_exit_table_result = docs_spec_semantic_reconciliation_result(
        "write",
        &json!({
            "path": "docs/spec.md",
            "content": "| CLI 入力 | 分類 | 終了コード |\n| --- | --- | --- |\n| `python tool.py log 10` | 不明な2トークン入力 | 1 |\n| `python tool.py 8 +` | 二項演算の不完全な入力 | 1 |"
        }),
        &state,
        Some(&active_work),
        Utf8Path::new("C:/workspace"),
        Some(
            "Docs only. Incomplete binary-looking `python tool.py 8 +` must be usage error exit code 1. Unknown two-token `python tool.py log 10` must be usage error exit code 1.",
        ),
    );
    result_ok
        && localized_result.as_ref().is_some_and(|tool_result| {
            tool_result.recorded_changes.is_empty()
                && tool_result.change_summaries.is_empty()
                && tool_result
                    .metadata
                    .pointer("/tool_feedback_envelope/side_effects_applied")
                    .and_then(Value::as_bool)
                    == Some(false)
                && tool_result
                    .metadata
                    .get("prohibited_claims_present")
                    .and_then(Value::as_array)
                    .is_some_and(|claims| {
                        claims.iter().any(|claim| {
                            claim.as_str()
                                == Some("unknown_two_token_cli_as_undefined_function_exit_2")
                        })
            })
        })
        && negative_guidance_result.is_none()
        && command_subject_result.is_none()
        && localized_numeric_exit_table_result.is_none()
        && docs_spec_semantic_reconciliation_result(
            "write",
            &json!({
                "path": "docs/spec.md",
                "content": "Unknown two-token CLI input is usage error exit code 1.\nUnsupported helper function is exit code 2."
            }),
            &state,
            Some(&active_work),
            Utf8Path::new("C:/workspace"),
            Some(authority),
        )
        .is_none()
        && docs_spec_semantic_reconciliation_result(
            "write",
            &json!({
                "path": "docs/spec.md",
                "content": "Inputs with fewer than 3 tokens are usage error exit code 1."
            }),
            &state,
            Some(&active_work),
            Utf8Path::new("C:/workspace"),
            Some("Docs only. Incomplete binary-looking `python tool.py 8 +` must be usage error exit code 1. Unknown two-token `python tool.py log 10` must be usage error exit code 1."),
        )
        .as_ref()
        .is_some_and(|tool_result| {
            tool_result.output_text.contains("Required claim detail")
                && tool_result.output_text.contains("python tool.py log 10")
                && tool_result
                    .output_text
                    .contains("Add a Markdown row or sentence containing `python tool.py log 10` with usage error semantics and exit code 1.")
                && tool_result
                    .metadata
                    .get("missing_required_claim_details")
                    .and_then(Value::as_array)
                    .is_some_and(|claims| {
                        claims.iter().any(|claim| {
                            claim
                                .get("evidence_refs")
                                .and_then(Value::as_array)
                                .is_some_and(|refs| {
                                    refs.iter().any(|value| {
                                        value
                                            .as_str()
                                            .is_some_and(|text| text.contains("python tool.py log 10"))
                                    })
                                })
                        })
                    })
                && tool_result
                    .metadata
                    .get("missing_required_claim_details")
                    .and_then(Value::as_array)
                    .is_some_and(|claims| {
                        claims.iter().any(|claim| {
                            claim
                                .get("repair_snippets")
                                .and_then(Value::as_array)
                                .is_some_and(|snippets| {
                                    snippets.iter().any(|value| {
                                        value.as_str().is_some_and(|text| {
                                            text.contains("python tool.py 8 +")
                                                && text.contains("usage error")
                                                && text.contains("exit code 1")
                                        })
                                    })
                                })
                        })
                    })
        })
}

fn documentation_semantic_contract_applies(
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
    latest_user_text: Option<&str>,
) -> bool {
    if latest_user_text.is_none() {
        return false;
    }
    if state.route == TaskRoute::Docs || state.docs_route.is_some() {
        return true;
    }
    if state
        .active_targets
        .iter()
        .any(|target| documentation_target(target))
    {
        return true;
    }
    match active_work {
        Some(ActiveWorkContract::DocsRepair { .. }) => true,
        Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        }) => pending_targets
            .iter()
            .any(|target| documentation_target(target)),
        _ => false,
    }
}

fn documentation_semantic_contract_from_authority(
    authority_text: &str,
) -> Option<DocumentationSemanticClaimContract> {
    let normalized = normalize_semantic_text(authority_text);
    let mut contract = DocumentationSemanticClaimContract::default();

    if mentions_unknown_two_token_cli_usage_error(&normalized) {
        let examples = extract_cli_code_examples(authority_text);
        contract.required_claims.push(DocumentationSemanticClaim {
            id: "unknown_two_token_cli_usage_error_exit_1",
            description:
                "Document that unknown two-token CLI input is a usage error with exit code 1."
                    .to_string(),
            evidence_refs: examples
                .iter()
                .filter(|example| {
                    let normalized = normalize_semantic_text(example);
                    normalized.contains("log 10")
                        || normalized.contains("unknown")
                        || normalized.contains("未知")
                        || normalized.contains("不明")
                        || normalized.contains("未定義")
                })
                .cloned()
                .collect(),
            subject_term_groups: vec![
                terms(&["unknown", "不明", "未定義", "未知"]),
                terms(&[
                    "two token",
                    "2 token",
                    "2トークン",
                    "2 トークン",
                    "二つの引数",
                ]),
            ],
            predicate_term_groups: vec![
                terms(&[
                    "usage",
                    "invalid",
                    "error",
                    "使い方",
                    "使用方法",
                    "不正",
                    "無効",
                    "入力",
                ]),
                terms(&["exit code 1", "終了コード 1"]),
            ],
        });
    }
    if mentions_incomplete_binary_cli_usage_error(&normalized) {
        let examples = extract_cli_code_examples(authority_text);
        contract.required_claims.push(DocumentationSemanticClaim {
            id: "incomplete_binary_cli_usage_error_exit_1",
            description:
                "Document that incomplete binary-looking CLI input is a usage error with exit code 1."
                    .to_string(),
            evidence_refs: examples
                .iter()
                .filter(|example| {
                    let normalized = normalize_semantic_text(example);
                    normalized.contains("8 +")
                        || normalized.contains("incomplete")
                        || normalized.contains("不完全")
                })
                .cloned()
                .collect(),
            subject_term_groups: vec![
                terms(&["binary", "二項"]),
                terms(&["incomplete", "不完全"]),
            ],
            predicate_term_groups: vec![
                terms(&[
                    "usage",
                    "invalid",
                    "incomplete",
                    "error",
                    "使い方",
                    "使用方法",
                    "不正",
                    "無効",
                    "不完全",
                    "入力",
                ]),
                terms(&["exit code 1", "終了コード 1"]),
            ],
        });
    }
    if prohibits_unknown_two_token_cli_unsupported_function_exit_2(&normalized) {
        contract.prohibited_claims.push(DocumentationSemanticClaim {
            id: "unknown_two_token_cli_as_unsupported_function_exit_2",
            description:
                "Do not document unknown two-token CLI input as unsupported function exit code 2."
                    .to_string(),
            evidence_refs: extract_cli_code_examples(authority_text),
            subject_term_groups: vec![
                terms(&["unknown", "不明", "未定義", "未知"]),
                terms(&[
                    "two token",
                    "two-token",
                    "2 token",
                    "2-token",
                    "2トークン",
                    "2 トークン",
                    "二つの引数",
                ]),
            ],
            predicate_term_groups: vec![
                terms(&[
                    "unsupported function",
                    "unsupported function",
                    "unknown unary function",
                ]),
                terms(&["exit code 2", "終了コード 2"]),
            ],
        });
        contract.prohibited_claims.push(DocumentationSemanticClaim {
            id: "unknown_two_token_cli_as_undefined_function_exit_2",
            description:
                "Do not document unknown two-token CLI input as undefined function exit code 2."
                    .to_string(),
            evidence_refs: extract_cli_code_examples(authority_text),
            subject_term_groups: vec![
                terms(&["unknown", "不明", "未定義", "未知"]),
                terms(&[
                    "two token",
                    "two-token",
                    "2 token",
                    "2-token",
                    "2トークン",
                    "2 トークン",
                    "二つの引数",
                ]),
            ],
            predicate_term_groups: vec![
                terms(&["未定義関数", "未知の単項関数", "未知単項関数"]),
                terms(&["exit code 2", "終了コード 2"]),
            ],
        });
    }

    (!contract.required_claims.is_empty() || !contract.prohibited_claims.is_empty())
        .then_some(contract)
}

fn reconcile_documentation_semantics(
    contract: &DocumentationSemanticClaimContract,
    document_text: &str,
) -> DocumentationSemanticReport {
    let required_claim_observations = contract
        .required_claims
        .iter()
        .map(|claim| {
            let observed_refs = semantic_claim_observed_refs(document_text, claim);
            (claim, observed_refs)
        })
        .collect::<Vec<_>>();
    let satisfied_required_claims = required_claim_observations
        .iter()
        .filter_map(|(claim, observed_refs)| {
            (!observed_refs.is_empty()).then(|| {
                SemanticClaimProjection::from_claim_with_observed_refs(claim, observed_refs.clone())
            })
        })
        .collect::<Vec<_>>();
    let missing_required_claims = required_claim_observations
        .iter()
        .filter_map(|(claim, observed_refs)| {
            observed_refs
                .is_empty()
                .then(|| SemanticClaimProjection::from_claim(claim))
        })
        .collect::<Vec<_>>();
    let prohibited_claims_present = contract
        .prohibited_claims
        .iter()
        .filter_map(|claim| {
            let observed_refs = prohibited_semantic_claim_observed_refs(document_text, claim);
            (!observed_refs.is_empty()).then(|| {
                SemanticClaimProjection::from_claim_with_observed_refs(claim, observed_refs)
            })
        })
        .collect::<Vec<_>>();
    DocumentationSemanticReport {
        satisfied_required_claims,
        missing_required_claims,
        prohibited_claims_present,
    }
}

impl SemanticClaimProjection {
    fn from_claim(claim: &DocumentationSemanticClaim) -> Self {
        Self {
            id: claim.id,
            description: claim.description.clone(),
            evidence_refs: claim.evidence_refs.clone(),
            observed_refs: Vec::new(),
            repair_snippets: semantic_claim_repair_snippets(claim),
        }
    }

    fn from_claim_with_observed_refs(
        claim: &DocumentationSemanticClaim,
        observed_refs: Vec<String>,
    ) -> Self {
        Self {
            id: claim.id,
            description: claim.description.clone(),
            evidence_refs: claim.evidence_refs.clone(),
            observed_refs,
            repair_snippets: semantic_claim_repair_snippets(claim),
        }
    }
}

fn semantic_claim_repair_snippets(claim: &DocumentationSemanticClaim) -> Vec<String> {
    let normalized_description = normalize_semantic_text(&claim.description);
    let mut snippets = claim
        .evidence_refs
        .iter()
        .filter(|evidence| !evidence.trim().is_empty())
        .map(|evidence| {
            if normalized_description.contains("usage error")
                && normalized_description.contains("exit code 1")
            {
                format!(
                    "Add a Markdown row or sentence containing `{}` with usage error semantics and exit code 1.",
                    evidence.trim()
                )
            } else if normalized_description.contains("exit code 2") {
                format!(
                    "Remove or rewrite any claim that maps `{}` to exit code 2.",
                    evidence.trim()
                )
            } else {
                format!(
                    "Document `{}` according to: {}",
                    evidence.trim(),
                    claim.description
                )
            }
        })
        .collect::<Vec<_>>();
    if snippets.is_empty() {
        snippets.push(format!(
            "Document claim `{}`: {}",
            claim.id, claim.description
        ));
    }
    snippets.sort();
    snippets.dedup();
    snippets
}

fn semantic_claim_observed_refs(
    document_text: &str,
    claim: &DocumentationSemanticClaim,
) -> Vec<String> {
    let mut refs = document_claim_segments(document_text)
        .into_iter()
        .filter_map(|segment| {
            let normalized = normalize_semantic_text(&segment);
            semantic_claim_segment_matches(&normalized, claim).then_some(segment)
        })
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    refs
}

fn prohibited_semantic_claim_observed_refs(
    document_text: &str,
    claim: &DocumentationSemanticClaim,
) -> Vec<String> {
    let mut refs = document_claim_segments(document_text)
        .into_iter()
        .filter_map(|segment| {
            let normalized = normalize_semantic_text(&segment);
            (semantic_claim_segment_matches(&normalized, claim)
                && !semantic_claim_segment_is_negative_or_preventive(&normalized))
            .then_some(segment)
        })
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    refs
}

fn semantic_claim_segment_is_negative_or_preventive(normalized_segment: &str) -> bool {
    [
        "do not",
        "don't",
        "must not",
        "should not",
        "not create",
        "not document",
        "not treat",
        "no generated test",
        "without",
        "禁止",
        "含めない",
        "作らない",
        "作成しない",
        "期待しない",
        "期待する生成 test は作成しない",
        "期待する生成テストは作成しない",
        "ではない",
        "しない",
        "ない",
    ]
    .iter()
    .any(|marker| normalized_segment.contains(marker))
}

fn semantic_claim_segment_matches(
    normalized_segment: &str,
    claim: &DocumentationSemanticClaim,
) -> bool {
    semantic_claim_subject_matches(normalized_segment, claim)
        && semantic_claim_predicate_matches(normalized_segment, claim)
}

fn semantic_claim_subject_matches(
    normalized_segment: &str,
    claim: &DocumentationSemanticClaim,
) -> bool {
    semantic_claim_evidence_ref_matches(normalized_segment, &claim.evidence_refs)
        || semantic_claim_term_groups_match(normalized_segment, &claim.subject_term_groups)
}

fn semantic_claim_predicate_matches(
    normalized_segment: &str,
    claim: &DocumentationSemanticClaim,
) -> bool {
    semantic_claim_term_groups_match(normalized_segment, &claim.predicate_term_groups)
}

fn semantic_claim_term_groups_match(normalized_text: &str, groups: &[Vec<String>]) -> bool {
    groups
        .iter()
        .all(|terms| terms.iter().any(|term| normalized_text.contains(term)))
}

fn document_claim_segments(document_text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut paragraph = Vec::new();
    for line in document_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            push_claim_segment(&mut segments, &mut paragraph);
            continue;
        }
        segments.push(trimmed.to_string());
        paragraph.push(trimmed.to_string());
        if paragraph.len() >= 4 {
            push_claim_segment(&mut segments, &mut paragraph);
        }
    }
    push_claim_segment(&mut segments, &mut paragraph);
    segments
}

fn push_claim_segment(segments: &mut Vec<String>, paragraph: &mut Vec<String>) {
    if paragraph.is_empty() {
        return;
    }
    segments.push(paragraph.join(" "));
    paragraph.clear();
}

fn semantic_claim_evidence_ref_matches(normalized_segment: &str, evidence_refs: &[String]) -> bool {
    evidence_refs.iter().any(|evidence| {
        let normalized_evidence = normalize_semantic_text(evidence);
        !normalized_evidence.is_empty()
            && normalized_evidence.split_whitespace().count() >= 2
            && normalized_segment.contains(&normalized_evidence)
    })
}

fn terms(values: &[&str]) -> Vec<String> {
    values
        .iter()
        .map(|value| normalize_semantic_text(value))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentationCandidate {
    targets: Vec<String>,
    content: String,
}

fn documentation_candidate_from_tool(
    tool_name: &str,
    arguments: &Value,
    workspace_root: &Utf8Path,
) -> Option<DocumentationCandidate> {
    match tool_name {
        "write" => {
            let path = normalize_doc_target(arguments.get("path")?.as_str()?, workspace_root)?;
            let content = arguments.get("content")?.as_str()?.to_string();
            Some(DocumentationCandidate {
                targets: vec![path],
                content,
            })
        }
        "apply_patch" => {
            let patch = arguments.get("patch_text")?.as_str()?;
            let targets = patch_documentation_targets(patch, workspace_root);
            let content = patch
                .lines()
                .filter_map(|line| {
                    line.strip_prefix('+')
                        .filter(|_| !line.starts_with("+++"))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!targets.is_empty() && !content.trim().is_empty())
                .then_some(DocumentationCandidate { targets, content })
        }
        _ => None,
    }
}

fn patch_documentation_targets(patch: &str, workspace_root: &Utf8Path) -> Vec<String> {
    let mut targets = BTreeSet::new();
    for line in patch.lines() {
        let Some(raw) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
        else {
            continue;
        };
        if let Some(target) = normalize_doc_target(raw.trim(), workspace_root)
            && documentation_target(Utf8Path::new(&target))
        {
            targets.insert(target);
        }
    }
    targets.into_iter().collect()
}

fn normalize_doc_target(path: &str, workspace_root: &Utf8Path) -> Option<String> {
    let candidate = Utf8Path::new(path.trim());
    let relative = if candidate.is_absolute() {
        candidate.strip_prefix(workspace_root).ok()?.to_path_buf()
    } else {
        candidate.to_path_buf()
    };
    Some(relative.as_str().replace('\\', "/"))
}

fn documentation_target(path: &Utf8Path) -> bool {
    let normalized = path.as_str().replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    normalized.contains("/docs/")
        || matches!(
            name,
            "readme.md"
                | "design.md"
                | "spec.md"
                | "basic_design.md"
                | "detail_design.md"
                | "detailed_design.md"
        )
        || name.ends_with(".md")
        || name.ends_with(".markdown")
}

fn mentions_unknown_two_token_cli_usage_error(normalized: &str) -> bool {
    mentions_unknown_two_token_cli(normalized)
        && (normalized.contains("usage error")
            || normalized.contains("usage")
            || normalized.contains("使い方")
            || normalized.contains("使用方法"))
        && normalized.contains("exit code 1")
}

fn mentions_incomplete_binary_cli_usage_error(normalized: &str) -> bool {
    (normalized.contains("8 +")
        || normalized.contains("incomplete binary")
        || normalized.contains("不完全"))
        && (normalized.contains("binary") || normalized.contains("二項"))
        && (normalized.contains("usage error")
            || normalized.contains("usage")
            || normalized.contains("使い方")
            || normalized.contains("使用方法"))
        && normalized.contains("exit code 1")
}

fn prohibits_unknown_two_token_cli_unsupported_function_exit_2(normalized: &str) -> bool {
    mentions_unknown_two_token_cli(normalized)
        && (normalized.contains("unsupported function")
            || normalized.contains("unsupported-function")
            || normalized.contains("未定義関数"))
        && (normalized.contains("exit code 2") || normalized.contains("exit-code-2"))
        && (normalized.contains("do not")
            || normalized.contains("not ")
            || normalized.contains("no ")
            || normalized.contains("禁止")
            || normalized.contains("含めない")
            || normalized.contains("作らない")
            || normalized.contains("ではない")
            || normalized.contains("ない"))
}

fn mentions_unknown_two_token_cli(normalized: &str) -> bool {
    let names_prompt_command_subject = normalized.contains("python ")
        && normalized.contains("log 10")
        && (normalized.contains("tool.py") || normalized.contains("calculator.py"));
    names_prompt_command_subject
        || ((normalized.contains("unknown")
            || normalized.contains("不明")
            || normalized.contains("未定義")
            || normalized.contains("未知"))
            && (normalized.contains("two token")
                || normalized.contains("two-token")
                || normalized.contains("2 token")
                || normalized.contains("2-token")
                || normalized.contains("2トークン")
                || normalized.contains("2 トークン")
                || normalized.contains("二つの引数")))
}

fn normalize_semantic_text(text: &str) -> String {
    let mut normalized = text.to_lowercase();
    for code in 0..=255 {
        let exit_code = format!("exit code {code}");
        for pattern in [
            format!("sys.exit({code})"),
            format!("sys.exit ({code})"),
            format!("exit({code})"),
            format!("exit ({code})"),
        ] {
            normalized = normalized.replace(&pattern, &exit_code);
        }
        normalized =
            normalized.replace(&format!("終了コード{code}"), &format!("終了コード {code}"));
        if semantic_text_has_exit_code_header(&normalized)
            && semantic_text_has_numeric_table_cell(&normalized, code)
        {
            normalized.push_str(&format!(" {exit_code} 終了コード {code}"));
        }
    }
    normalized
        .replace("exit code:", "exit code ")
        .replace("未定義の関数", "未定義関数")
        .replace("未定義 の 関数", "未定義関数")
        .replace("未知の単項関数", "未知の単項関数")
        .replace("未知 の 単項 関数", "未知の単項関数")
        .replace('`', " ")
        .replace('_', " ")
        .replace('-', " ")
        .replace('\r', "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn semantic_text_has_exit_code_header(normalized: &str) -> bool {
    normalized.contains("exit code")
        || normalized.contains("終了コード")
        || normalized.contains("| exit |")
        || normalized.contains("|終了|")
        || normalized.contains("| 終了 |")
}

fn semantic_text_has_numeric_table_cell(normalized: &str, code: u16) -> bool {
    [
        format!("| {code} |"),
        format!("|{code}|"),
        format!("| {code}\n"),
        format!("|{code}\n"),
        format!("| {code}\r"),
        format!("|{code}\r"),
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
}

fn extract_cli_code_examples(text: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let mut in_code = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if in_code {
                let candidate = current.trim();
                if !candidate.is_empty()
                    && (candidate.contains(".py") || candidate.split_whitespace().count() <= 4)
                {
                    examples.push(candidate.to_string());
                }
                current.clear();
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            current.push(ch);
        }
    }
    examples.sort();
    examples.dedup();
    examples
}

fn docs_spec_semantic_reconciliation_tool_result(
    tool_name: &str,
    arguments: &Value,
    candidate: DocumentationCandidate,
    report: DocumentationSemanticReport,
) -> ToolResult {
    let missing = report
        .missing_required_claims
        .iter()
        .map(|value| value.id.to_string())
        .collect::<Vec<_>>();
    let prohibited = report
        .prohibited_claims_present
        .iter()
        .map(|value| value.id.to_string())
        .collect::<Vec<_>>();
    let missing_details = report
        .missing_required_claims
        .iter()
        .map(semantic_claim_detail_json)
        .collect::<Vec<_>>();
    let satisfied_details = report
        .satisfied_required_claims
        .iter()
        .map(semantic_claim_detail_json)
        .collect::<Vec<_>>();
    let prohibited_details = report
        .prohibited_claims_present
        .iter()
        .map(semantic_claim_detail_json)
        .collect::<Vec<_>>();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    candidate.targets.hash(&mut hasher);
    missing.hash(&mut hasher);
    prohibited.hash(&mut hasher);
    let result_hash = format!("docs-spec-semantic-reconciliation-{:016x}", hasher.finish());
    ToolResult {
        title: "Docs/spec semantic reconciliation failed".to_string(),
        output_text: format!(
            "Runtime rejected `{tool_name}` before filesystem side effects because the documentation/spec draft does not reconcile with the latest request authority. Missing required claim(s): {}. Required claim detail(s): {}. Prohibited claim(s) present: {}. Prohibited claim detail(s): {}. Rewrite the same documentation target so required claims are present and prohibited or contradictory claims are removed before closeout.",
            if missing.is_empty() {
                "none".to_string()
            } else {
                missing.join(", ")
            },
            semantic_claim_details_text(&report.missing_required_claims),
            if prohibited.is_empty() {
                "none".to_string()
            } else {
                prohibited.join(", ")
            },
            semantic_claim_details_text(&report.prohibited_claims_present),
        ),
        metadata: json!({
            "success": false,
            "docs_spec_semantic_reconciliation": true,
            "requested_tool": tool_name,
            "requested_arguments": arguments,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "docs_spec_semantic_reconciliation_failed",
            "progress_effect": "no_progress",
            "targets": candidate.targets,
            "missing_required_claims": missing,
            "missing_required_claim_details": missing_details,
            "satisfied_required_claim_details": satisfied_details,
            "prohibited_claims_present": prohibited,
            "prohibited_claim_details": prohibited_details,
            "result_hash": result_hash,
            "tool_feedback_envelope": {
                "kind": "docs_spec_semantic_reconciliation_failed",
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "docs_spec_semantic_reconciliation_failed",
                "progress_effect": "no_progress",
                "side_effects_applied": false,
                "result_hash": result_hash
            },
            "terminal_guard_policy": {
                "owner": "tool_lifecycle_runtime",
                "no_progress_guard": true,
                "side_effects_applied": false,
                "terminal_after_repeated_corrections": DOCS_SPEC_SEMANTIC_RECONCILIATION_TERMINAL_THRESHOLD
            }
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn semantic_claim_detail_json(claim: &SemanticClaimProjection) -> Value {
    json!({
        "id": claim.id,
        "description": claim.description,
        "evidence_refs": claim.evidence_refs,
        "observed_refs": claim.observed_refs,
        "repair_snippets": claim.repair_snippets,
    })
}

fn semantic_claim_details_text(claims: &[SemanticClaimProjection]) -> String {
    if claims.is_empty() {
        return "none".to_string();
    }
    claims
        .iter()
        .map(|claim| {
            let evidence = if claim.evidence_refs.is_empty() {
                "latest user authority".to_string()
            } else {
                claim.evidence_refs.join(" | ")
            };
            let observed = if claim.observed_refs.is_empty() {
                "none".to_string()
            } else {
                claim.observed_refs.join(" | ")
            };
            let repair = if claim.repair_snippets.is_empty() {
                "none".to_string()
            } else {
                claim.repair_snippets.join(" | ")
            };
            format!(
                "{}: {} Evidence: {} Observed: {} Required repair snippet(s): {}",
                claim.id, claim.description, evidence, observed, repair
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn latest_user_authority_text_uses_sequence_order() {
        assert!(super::latest_user_authority_text_uses_sequence_order_fixture_passes());
    }
}
