use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolEffectClass, ToolEffectPolicy, ToolName, ToolResult, ToolSpec};
use crate::workspace::AccessKind;

/// Installed only on a Runner's authenticated shared assignment. The Hub validates
/// that its current attempt and the target lease belong to the same conversation.
pub(crate) struct SharedStopServiceTool {
    pub job_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub candidates: Vec<crate::runner::shared::SharedCandidate>,
}

pub(crate) struct SharedServicesTool {
    pub job_id: String,
    pub attempt_id: String,
    pub generation: u64,
}

#[async_trait(?Send)]
impl Tool for SharedServicesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedServices,
            effect: ToolEffectPolicy::read(),
            description: "List finite servers retained for this exact shared conversation, including another PC. Use their service_id for shared_stop_service and distinguish running, stop requested, and uncertain states. A retained process alone does not prove the app is reachable.",
            input_schema: json!({"type":"object","additionalProperties":false}),
        }
    }

    async fn execute(
        &self,
        raw: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        if !raw.is_object() || raw.as_object().is_some_and(|value| !value.is_empty()) {
            return Err(ToolError::Message(
                "shared_services takes no arguments".into(),
            ));
        }
        ctx.run_mutation_fence.assert_owned().await?;
        ctx.services
            .store
            .session_repo()
            .require_active_shared_tool(
                &self.job_id,
                ctx.session.session.id,
                ctx.run_mutation_fence.turn_id(),
            )?;
        let network =
            ctx.services.store.device_network().ok_or_else(|| {
                ToolError::Message("This Runner has no Hub device connection".into())
            })?;
        let value = network
            .shared_conversation_services(&self.attempt_id, self.generation)
            .await
            .map_err(ToolError::Message)?;
        Ok(ToolResult {
            title: "Servers kept for this conversation".into(),
            output_text: serde_json::to_string_pretty(&value)?,
            metadata: value,
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    service_id: String,
}

fn stop_permission_details(
    service_id: &str,
    services: &serde_json::Value,
    candidates: &[crate::runner::shared::SharedCandidate],
) -> Result<Vec<String>, ToolError> {
    let environment_id = services
        .as_array()
        .and_then(|services| {
            services
                .iter()
                .find(|service| service["service_id"].as_str() == Some(service_id))
        })
        .and_then(|service| service["environment_id"].as_str())
        .filter(|id| crate::device_network::stable_id(id))
        .ok_or_else(|| {
            ToolError::Message(
                "この会話で停止できるアプリが見つかりません。起動状況を確認してください。".into(),
            )
        })?;
    // Display labels come from the Hub assignment, never from model arguments.
    // The exact service ID stays visible even when its PC is no longer a candidate.
    // These strings describe the operation; Hub remains its authorization owner.
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.environment_id == environment_id);
    let pc = candidate
        .map(|candidate| candidate.device_label.trim())
        .filter(|label| !label.is_empty());
    let environment = candidate
        .map(|candidate| candidate.environment_label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or(environment_id);
    let target = match pc {
        Some(pc) => format!("{pc} の起動中アプリ（ID: {service_id}）"),
        None => format!("実行環境 {environment_id} の起動中アプリ（ID: {service_id}）"),
    };
    Ok(vec![
        "操作: 起動中アプリの停止".into(),
        format!("対象: {target}"),
        format!("実行環境: {environment}"),
        "このアプリに停止を依頼します。作成したファイルは削除しません。停止完了は実行PCからの応答で確認します。".into(),
    ])
}

#[async_trait(?Send)]
impl Tool for SharedStopServiceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedStopService,
            effect: ToolEffectPolicy::mutation(),
            description: "Request an exact test server from this shared conversation to stop, including on another PC. Use the service_id returned when it was started or retained. Hub checks the current conversation and owner; a successful response is only a stop request. Verify the server has actually stopped before claiming cleanup.",
            input_schema: json!({"type":"object","additionalProperties":false,
                "required":["service_id"],"properties":{"service_id":{"type":"string"}}}),
        }
    }

    async fn execute(
        &self,
        raw: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: Input = serde_json::from_value(raw)?;
        if !crate::device_network::stable_id(&input.service_id) {
            return Err(ToolError::Message(
                "Invalid retained service identity".into(),
            ));
        }
        ctx.run_mutation_fence.assert_owned().await?;
        ctx.services
            .store
            .session_repo()
            .require_active_shared_tool(
                &self.job_id,
                ctx.session.session.id,
                ctx.run_mutation_fence.turn_id(),
            )?;
        let network =
            ctx.services.store.device_network().ok_or_else(|| {
                ToolError::Message("This Runner has no Hub device connection".into())
            })?;
        let services = network
            .shared_conversation_services(&self.attempt_id, self.generation)
            .await
            .map_err(ToolError::Message)?;
        let details = stop_permission_details(&input.service_id, &services, &self.candidates)?;
        let admission = ctx
            .confirm_if_needed_with_details(
                AccessKind::Edit,
                "この会話で起動したアプリを停止します。".into(),
                details,
                Vec::new(),
                false,
                ToolEffectClass::Mutation.permission_risks(),
            )
            .await?;
        admission.admit()?;
        let _commit = ctx.run_mutation_fence.begin_effect_commit()?;
        let value = network
            .shared_conversation_stop(&input.service_id, &self.attempt_id, self.generation)
            .await
            .map_err(ToolError::Message)?;
        Ok(ToolResult {
            title: "アプリの停止を依頼しました".into(),
            output_text: serde_json::to_string_pretty(&value)?,
            metadata: value,
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_permission_identifies_the_exact_service_and_hub_pc() {
        let services = json!([
            {"service_id":"other-app","environment_id":"win-a"},
            {"service_id":"todo-app","environment_id":"win-b"}
        ]);
        let candidates = vec![crate::runner::shared::SharedCandidate {
            environment_id: "win-b".into(),
            device_id: "device-b".into(),
            device_label: "WinB".into(),
            environment_label: "TODOアプリの作成".into(),
            capabilities: Vec::new(),
        }];
        let details = stop_permission_details("todo-app", &services, &candidates).unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("WinB") && detail.contains("todo-app"))
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("TODOアプリの作成"))
        );
        assert!(!details.iter().any(|detail| detail.contains("other-app")));
    }

    #[test]
    fn stop_permission_keeps_exact_target_when_pc_label_is_unavailable() {
        let services = json!([{"service_id":"todo-app","environment_id":"win-b"}]);
        let details = stop_permission_details("todo-app", &services, &[]).unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("win-b") && detail.contains("todo-app"))
        );
    }

    #[test]
    fn stop_permission_rejects_a_service_missing_from_the_conversation() {
        let services = json!([{"service_id":"other-app","environment_id":"win-b"}]);
        assert!(stop_permission_details("todo-app", &services, &[]).is_err());
        assert!(stop_permission_details("todo-app", &json!([]), &[]).is_err());
        assert!(
            stop_permission_details("todo-app", &json!([{"service_id":"todo-app"}]), &[]).is_err()
        );
    }
}
