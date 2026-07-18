use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ToolError;
use crate::protocol::{PlanStep, PlanStepStatus};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlanInput {
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<PlanStep>,
}

#[derive(Debug, Default)]
pub struct UpdatePlanTool;

#[async_trait(?Send)]
impl Tool for UpdatePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::UpdatePlan,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Update the client-visible plan for a non-trivial task. Keep steps concise, ordered, and current as work completes or the approach changes. The plan is progress projection only and does not replace doing or verifying the requested work.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["plan"],
                "properties": {
                    "explanation": {
                        "type": "string",
                        "description": "Optional short explanation of why the plan changed."
                    },
                    "plan": {
                        "type": "array",
                        "description": "The complete updated plan in logical execution order.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["step", "status"],
                            "properties": {
                                "step": {
                                    "type": "string",
                                    "description": "A concise, verifiable step."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            }
                        }
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = canonical_update_plan_input(raw_arguments)?;
        Ok(update_plan_result(input))
    }
}

fn canonical_update_plan_input(
    raw_arguments: serde_json::Value,
) -> Result<UpdatePlanInput, ToolError> {
    let mut input = serde_json::from_value::<UpdatePlanInput>(raw_arguments)?;
    input.explanation = input
        .explanation
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    for item in &mut input.plan {
        item.step = item.step.trim().to_string();
        if item.step.is_empty() {
            return Err(ToolError::Message(
                "update_plan requires every step to be non-empty".to_string(),
            ));
        }
    }

    let in_progress = input
        .plan
        .iter()
        .filter(|item| item.status == PlanStepStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err(ToolError::Message(
            "update_plan accepts at most one `in_progress` step".to_string(),
        ));
    }

    Ok(input)
}

fn update_plan_result(input: UpdatePlanInput) -> ToolResult {
    ToolResult {
        title: "Plan updated".to_string(),
        output_text: "Plan updated".to_string(),
        metadata: json!({
            "explanation": input.explanation,
            "plan": input.plan,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_plan_exposes_only_the_canonical_schema() {
        let spec = UpdatePlanTool.spec();
        assert_eq!(spec.name, ToolName::UpdatePlan);
        assert_eq!(spec.input_schema["required"], json!(["plan"]));
        assert_eq!(
            spec.input_schema["properties"]["plan"]["items"]["required"],
            json!(["step", "status"])
        );
    }

    #[test]
    fn update_plan_metadata_preserves_the_structured_projection() {
        let input = canonical_update_plan_input(json!({
            "explanation": "  Evidence changed the next step  ",
            "plan": [
                {"step": " Inspect the relevant contracts ", "status": "completed"},
                {"step": "Implement the coherent change", "status": "in_progress"},
                {"step": "Verify the outcome", "status": "pending"}
            ]
        }))
        .expect("plan");
        let result = update_plan_result(input);
        assert_eq!(result.output_text, "Plan updated");
        assert_eq!(
            result.metadata["explanation"],
            "Evidence changed the next step"
        );
        assert_eq!(
            result.metadata["plan"][0]["step"],
            "Inspect the relevant contracts"
        );
    }

    #[test]
    fn update_plan_rejects_noncanonical_statuses_and_multiple_active_steps() {
        assert!(
            canonical_update_plan_input(json!({
                "plan": [{"step": "Wait for input", "status": "blocked"}]
            }))
            .is_err()
        );
        assert!(
            canonical_update_plan_input(json!({
                "plan": [
                    {"step": "First", "status": "in_progress"},
                    {"step": "Second", "status": "in_progress"}
                ]
            }))
            .is_err()
        );
    }
}
