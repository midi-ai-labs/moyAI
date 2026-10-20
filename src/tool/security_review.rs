use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::protocol::ToolLifecycleStatus;
use crate::storage::session_repo::{
    MAX_SECURITY_REVIEW_CHECK_REFERENCE_BYTES as MAX_CHECK_ID_BYTES,
    MAX_SECURITY_REVIEW_CHECKS as MAX_CHECKS, SecurityReviewCheck, SecurityReviewEvidence,
};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};

const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_SUMMARY_BYTES: usize = 2048;
const MAX_ITEMS: usize = 16;
const MAX_ITEM_BYTES: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityReviewInput {
    summary: String,
    findings: Vec<String>,
    limitations: Vec<String>,
    check_tool_call_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SecurityReviewTool;

#[async_trait(?Send)]
impl Tool for SecurityReviewTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SecurityReview,
            effect: ToolEffectPolicy::read(),
            description: "Record your security review for the current turn. Cite completed check operation IDs; canonical tool-call IDs or unambiguous provider tool-call IDs are accepted. If IDs are absent from visible text, first use read_history: query distinctive check command text or its tool name (case-sensitive string-value search; tool_call finds call records), follow next_after, select the exact operation, then read matches[].item_id with item_id. Copy call_id from the returned text JSON, following next if incomplete. item_id is not call_id. Do not invent IDs or reuse an older turn's command. A call record alone is not a successful result. Separate findings and unverified limitations. The host validates referenced identity, outcome and ordering, not review adequacy. Bookkeeping alone is not a check. If a current check cannot be identified, state the missing evidence in limitations. Later mutations make this review stale. This record is not a safety guarantee or permission approval.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["summary", "findings", "limitations", "check_tool_call_ids"],
                "properties": {
                    "summary": {"type": "string", "minLength": 1, "maxLength": MAX_SUMMARY_BYTES,
                        "description": "Observed review scope and conclusion; at most 2048 UTF-8 bytes."},
                    "findings": {"type": "array", "maxItems": MAX_ITEMS,
                        "items": {"type": "string", "minLength": 1, "maxLength": MAX_ITEM_BYTES},
                        "description": "Remaining security findings; each at most 1024 UTF-8 bytes. Empty means no finding reported, not proof of safety."},
                    "limitations": {"type": "array", "maxItems": MAX_ITEMS,
                        "items": {"type": "string", "minLength": 1, "maxLength": MAX_ITEM_BYTES},
                        "description": "Unknowns, checks not run and scope limitations; each at most 1024 UTF-8 bytes."},
                    "check_tool_call_ids": {"type": "array", "maxItems": MAX_CHECKS, "uniqueItems": true,
                        "items": {"type": "string", "minLength": 1, "maxLength": MAX_CHECK_ID_BYTES},
                        "description": "IDs of settled check operations in this turn. Failed checks stay failed; pending calls, another turn and security_review itself cannot be cited."}
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = validate_input(raw_arguments)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::RunInterrupted);
        }
        ctx.run_mutation_fence.assert_owned().await?;
        let evidence = ctx.services.store.session_repo().security_review_evidence(
            ctx.session.session.id,
            ctx.run_mutation_fence.turn_id(),
            &input.check_tool_call_ids,
        )?;
        review_result(input, evidence)
    }
}

fn validate_input(raw: Value) -> Result<SecurityReviewInput, ToolError> {
    if serde_json::to_vec(&raw)?.len() > MAX_BODY_BYTES {
        return Err(invalid("security_review input exceeds 16384 UTF-8 bytes"));
    }
    let input: SecurityReviewInput = serde_json::from_value(raw)?;
    if input.summary.trim().is_empty()
        || input.summary.len() > MAX_SUMMARY_BYTES
        || input.findings.len() > MAX_ITEMS
        || input.limitations.len() > MAX_ITEMS
        || input
            .findings
            .iter()
            .chain(&input.limitations)
            .any(|text| text.trim().is_empty() || text.len() > MAX_ITEM_BYTES)
        || input.check_tool_call_ids.len() > MAX_CHECKS
        || input
            .check_tool_call_ids
            .iter()
            .any(|id| id.trim().is_empty() || id.len() > MAX_CHECK_ID_BYTES)
    {
        return Err(invalid(
            "security_review arguments exceed their advertised bounds",
        ));
    }
    Ok(input)
}

fn invalid(message: &str) -> ToolError {
    ToolError::Message(message.into())
}

fn review_result(
    input: SecurityReviewInput,
    evidence: SecurityReviewEvidence,
) -> Result<ToolResult, ToolError> {
    let effect_position = evidence.state.latest_effect_position;
    let mut successful: Vec<SecurityReviewCheck> = Vec::new();
    let mut failed = Vec::new();
    let mut unverified = Vec::new();
    for check in evidence.checks {
        if check.is_successful_current_check(effect_position) {
            successful.push(check);
        } else if check.status == ToolLifecycleStatus::Failed || check.success == Some(false) {
            failed.push(check);
        } else {
            unverified.push(check);
        }
    }
    if successful.is_empty() && input.limitations.is_empty() {
        return Err(invalid(
            "security_review requires a limitation when no successful current check is established",
        ));
    }
    let check_count = successful.len() + failed.len() + unverified.len();
    let metadata = json!({
        "success": true,
        "reviewed_effect_position": effect_position,
        "reviewed_effect_item_id": evidence.state.latest_effect_item_id,
        "check_count": check_count,
        "successful_check_count": successful.len(),
        "finding_count": input.findings.len(),
        "limitation_count": input.limitations.len(),
    });
    let output_text = serde_json::to_string(&json!({
        "record": "security_review",
        "assurance": "review record only; not a safety guarantee or permission approval",
        "summary": input.summary,
        "findings": input.findings,
        "limitations": input.limitations,
        "reviewed_effect_position": effect_position,
        "reviewed_effect_item_id": evidence.state.latest_effect_item_id,
        "unclassified_tool_count": evidence.state.unclassified_tool_count,
        "successful_checks": successful,
        "failed_checks": failed,
        "unverified_checks": unverified,
    }))?;
    let result = ToolResult {
        title: "Security review recorded".into(),
        output_text,
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    };
    if serde_json::to_vec(&result)?.len() > MAX_BODY_BYTES {
        return Err(invalid(
            "security_review record exceeds 16384 UTF-8 bytes; shorten review text or cite fewer checks",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HistoryItemId;
    use crate::session::ToolCallId;
    use crate::storage::session_repo::SecurityReviewState;

    fn input(limitations: Vec<String>) -> SecurityReviewInput {
        validate_input(
            json!({"summary":"Reviewed the available evidence", "findings":[],
            "limitations":limitations, "check_tool_call_ids":[]}),
        )
        .unwrap()
    }

    fn evidence(checks: Vec<SecurityReviewCheck>) -> SecurityReviewEvidence {
        SecurityReviewEvidence {
            state: SecurityReviewState {
                latest_effect_position: Some(10),
                latest_effect_item_id: Some(HistoryItemId::new()),
                latest_review_position: None,
                reviewed_effect_position: None,
                reviewed_effect_item_id: None,
                check_count: 0,
                successful_check_count: 0,
                finding_count: 0,
                limitation_count: 0,
                unclassified_tool_count: 0,
            },
            checks,
        }
    }

    fn check(
        status: ToolLifecycleStatus,
        success: Option<bool>,
        position: i64,
    ) -> SecurityReviewCheck {
        SecurityReviewCheck {
            call_id: ToolCallId::new(),
            history_item_id: HistoryItemId::new(),
            append_position: position,
            tool_name: "shell".into(),
            status,
            success,
            security_effect: Some("destructive".into()),
        }
    }

    #[test]
    fn security_review_separates_success_failure_and_unverified_checks() {
        let result = review_result(
            input(vec!["Earlier checks predate the latest change".into()]),
            evidence(vec![
                check(ToolLifecycleStatus::Completed, Some(true), 10),
                check(ToolLifecycleStatus::Failed, Some(false), 11),
                check(ToolLifecycleStatus::Completed, Some(true), 9),
                check(ToolLifecycleStatus::Completed, None, 12),
            ]),
        )
        .unwrap();
        let body: Value = serde_json::from_str(&result.output_text).unwrap();
        assert_eq!(body["successful_checks"].as_array().unwrap().len(), 1);
        assert_eq!(body["failed_checks"].as_array().unwrap().len(), 1);
        assert_eq!(body["unverified_checks"].as_array().unwrap().len(), 2);
        assert_eq!(result.metadata["reviewed_effect_position"], 10);
        assert_eq!(result.metadata["successful_check_count"], 1);
        assert_eq!(result.metadata["check_count"], 4);
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_BODY_BYTES);
    }

    #[test]
    fn a_declaration_without_successful_checks_requires_an_explicit_limitation() {
        assert!(review_result(input(Vec::new()), evidence(Vec::new())).is_err());
        let result = review_result(
            input(vec!["No checks were run".into()]),
            evidence(Vec::new()),
        )
        .unwrap();
        assert_eq!(result.metadata["successful_check_count"], 0);
        assert_eq!(result.metadata["limitation_count"], 1);
    }

    #[test]
    fn security_review_enforces_utf8_counts_full_body_and_unknown_field_bounds() {
        let valid = json!({"summary":"review", "findings":[], "limitations":["not run"], "check_tool_call_ids":[]});
        for (key, value) in [
            ("summary", json!("あ".repeat(683))),
            ("findings", json!(vec!["finding"; 17])),
            ("limitations", json!(["x".repeat(1025)])),
            ("check_tool_call_ids", json!(vec!["id"; 33])),
            ("reviewed_effect_position", json!(999)),
        ] {
            let mut raw = valid.clone();
            raw[key] = value;
            assert!(validate_input(raw).is_err());
        }
        let mut oversized = valid;
        oversized["findings"] = json!(vec!["x".repeat(1024); 16]);
        assert!(validate_input(oversized).is_err());
    }

    #[test]
    fn valid_input_is_rejected_when_escaped_review_evidence_exceeds_output_bound() {
        let references = (0..32)
            .map(|_| ToolCallId::new().to_string())
            .collect::<Vec<_>>();
        let raw = json!({"summary":"\"".repeat(2048), "findings":["\\".repeat(1024)], "limitations":[], "check_tool_call_ids":references});
        let input = validate_input(raw.clone()).expect("the input itself fits");
        assert!(serde_json::to_vec(&raw).unwrap().len() < MAX_BODY_BYTES);
        let checks = input
            .check_tool_call_ids
            .iter()
            .map(|id| {
                let mut check = check(ToolLifecycleStatus::Completed, Some(true), 10);
                check.call_id = id.parse().unwrap();
                check
            })
            .collect();
        let error = review_result(input, evidence(checks))
            .expect_err("escaped canonical metadata exceeds the output bound");
        assert!(error.to_string().contains("record exceeds 16384"));
    }
}
