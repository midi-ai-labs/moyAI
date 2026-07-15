use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AgentForkTurns, AgentRunContext};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
const MIN_WAIT_TIMEOUT_MS: u64 = 10_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 3_600_000;

#[derive(Debug, Default)]
pub struct SpawnAgentTool;

#[derive(Debug, Default)]
pub struct SendMessageTool;

#[derive(Debug, Default)]
pub struct FollowupTaskTool;

#[derive(Debug, Default)]
pub struct WaitAgentTool;

#[derive(Debug, Default)]
pub struct InterruptAgentTool;

#[derive(Debug, Default)]
pub struct ListAgentsTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentInput {
    task_name: String,
    message: String,
    #[serde(default)]
    fork_turns: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    target: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitAgentInput {
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterruptAgentInput {
    target: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAgentsInput {
    #[serde(default)]
    path_prefix: Option<String>,
}

#[async_trait(?Send)]
impl Tool for SpawnAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SpawnAgent,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Spawn an agent for a concrete, bounded task that can run independently alongside useful local work. The child has a canonical task path and the same workspace and permissions. This medium profile supports one child level, so only the root agent can spawn.",
            input_schema: json!({
                "type": "object",
                "required": ["task_name", "message"],
                "additionalProperties": false,
                "properties": {
                    "task_name": {
                        "type": "string",
                        "description": "Task name for the new agent. Use lowercase letters, digits, and underscores."
                    },
                    "message": {
                        "type": "string",
                        "description": "Initial plain-text task for the new agent."
                    },
                    "fork_turns": {
                        "type": "string",
                        "enum": ["none", "all"],
                        "description": "Context to fork. Defaults to `all`; use `none` to start without surrounding conversation context."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<SpawnAgentInput>(raw_arguments)?;
        require_message("spawn_agent", &input.message)?;
        let fork_turns = parse_fork_turns(input.fork_turns.as_ref())?;
        let activity_id = ctx.tool_call_id.to_string();
        ctx.run_mutation_fence.assert_owned().await?;
        let agent = require_agent_context("spawn_agent", ctx.agent)?;
        let snapshot = agent
            .spawn_agent(
                &input.task_name,
                input.message,
                fork_turns,
                activity_id.clone(),
            )
            .await
            .map_err(ToolError::Message)?;
        let output = json!({
            "task_name": snapshot.path,
        });
        let metadata = json!({
            "activity_id": activity_id,
            "agent_path": snapshot.path,
            "session_id": snapshot.session_id,
            "status": snapshot.status,
            "agent": snapshot,
        });
        json_result("Agent spawned", output, metadata)
    }
}

#[async_trait(?Send)]
impl Tool for SendMessageTool {
    fn spec(&self) -> ToolSpec {
        message_spec(
            ToolName::SendMessage,
            "Send a message to an existing agent. The message is queued promptly and does not trigger a new turn.",
            "Relative or canonical task name to message (from spawn_agent).",
            "Message text to queue on the target agent.",
        )
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        send_message(raw_arguments, ctx, false, "send_message").await
    }
}

#[async_trait(?Send)]
impl Tool for FollowupTaskTool {
    fn spec(&self) -> ToolSpec {
        message_spec(
            ToolName::FollowupTask,
            "Send a follow-up task to an existing non-root agent and trigger a turn if it is idle. If it is already running, the task is delivered at the next safe message boundary.",
            "Agent id or canonical task name to send a follow-up task to (from spawn_agent).",
            "Message text to send to the target agent.",
        )
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        send_message(raw_arguments, ctx, true, "followup_task").await
    }
}

#[async_trait(?Send)]
impl Tool for WaitAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::WaitAgent,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Wait for a mailbox update from any live agent. The wait also ends early when new user input is steered into the active turn. Returns only an activity, interruption, or timeout summary, never hidden reasoning or message content.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": MIN_WAIT_TIMEOUT_MS,
                        "maximum": MAX_WAIT_TIMEOUT_MS,
                        "description": "Timeout in milliseconds. Defaults to 30000, min 10000, max 3600000."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<WaitAgentInput>(raw_arguments)?;
        let timeout_ms = validated_timeout(input.timeout_ms)?;
        let agent = require_agent_context("wait_agent", ctx.agent)?;
        let active_runs = ctx.services.store.active_runs().clone();
        let session_id = ctx.session.session.id;
        let steer_generation = active_runs
            .steer_generation(session_id)
            .map_err(|error| ToolError::Message(error.to_string()))?;
        let result = tokio::select! {
            result = agent.wait_for_activity(timeout_ms) => {
                result.map_err(ToolError::Message)?
            }
            steer = active_runs.wait_for_steer_activity(session_id, steer_generation) => {
                steer.map_err(|error| ToolError::Message(error.to_string()))?;
                crate::app::AgentWaitResult {
                    message: "Wait interrupted by new user input.".to_string(),
                    timed_out: false,
                    updated_agents: Vec::new(),
                }
            }
        };
        let output = serde_json::to_value(&result)?;
        json_result("Agent wait completed", output.clone(), output)
    }
}

#[async_trait(?Send)]
impl Tool for InterruptAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::InterruptAgent,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Interrupt an agent's current turn, if any, and return its previous status. The agent remains available for messages and follow-up tasks.",
            input_schema: json!({
                "type": "object",
                "required": ["target"],
                "additionalProperties": false,
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Agent id or canonical task name to interrupt (from spawn_agent)."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<InterruptAgentInput>(raw_arguments)?;
        let activity_id = ctx.tool_call_id.to_string();
        ctx.run_mutation_fence.assert_owned().await?;
        let agent = require_agent_context("interrupt_agent", ctx.agent)?;
        let status = agent
            .interrupt_agent(&input.target, activity_id.clone())
            .map_err(ToolError::Message)?;
        let output = json!({
            "agent_path": input.target,
            "status": status,
        });
        let metadata = json!({
            "activity_id": activity_id,
            "agent_path": input.target,
            "status": status,
        });
        json_result("Agent interrupted", output, metadata)
    }
}

#[async_trait(?Send)]
impl Tool for ListAgentsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::ListAgents,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "List live agents in the current root thread tree. Optionally filter by task-path prefix.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path_prefix": {
                        "type": "string",
                        "description": "Task-path prefix filter without a trailing slash. Omit to list all live agents."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<ListAgentsInput>(raw_arguments)?;
        let agent = require_agent_context("list_agents", ctx.agent)?;
        let agents = agent
            .list_agents(input.path_prefix.as_deref())
            .map_err(ToolError::Message)?;
        let output = json!({ "agents": agents });
        json_result("Agents listed", output.clone(), output)
    }
}

async fn send_message(
    raw_arguments: Value,
    ctx: ToolContext<'_>,
    trigger_turn: bool,
    tool_name: &'static str,
) -> Result<ToolResult, ToolError> {
    let input = serde_json::from_value::<MessageInput>(raw_arguments)?;
    require_message(tool_name, &input.message)?;
    let activity_id = ctx.tool_call_id.to_string();
    ctx.run_mutation_fence.assert_owned().await?;
    let agent = require_agent_context(tool_name, ctx.agent)?;
    agent
        .send_message(
            &input.target,
            input.message,
            trigger_turn,
            activity_id.clone(),
        )
        .await
        .map_err(ToolError::Message)?;
    let output = json!({
        "agent_path": input.target,
        "queued": true,
        "trigger_turn": trigger_turn,
    });
    let metadata = json!({
        "activity_id": activity_id,
        "agent_path": input.target,
        "queued": true,
        "trigger_turn": trigger_turn,
    });
    json_result(
        if trigger_turn {
            "Follow-up task queued"
        } else {
            "Message queued"
        },
        output,
        metadata,
    )
}

fn message_spec(
    name: ToolName,
    description: &'static str,
    target_description: &'static str,
    message_description: &'static str,
) -> ToolSpec {
    ToolSpec {
        name,
        effect: crate::tool::ToolEffectPolicy::mutation(),
        description,
        input_schema: json!({
            "type": "object",
            "required": ["target", "message"],
            "additionalProperties": false,
            "properties": {
                "target": {
                    "type": "string",
                    "description": target_description
                },
                "message": {
                    "type": "string",
                    "description": message_description
                }
            }
        }),
    }
}

fn require_agent_context<'a>(
    tool_name: &str,
    agent: Option<&'a AgentRunContext>,
) -> Result<&'a AgentRunContext, ToolError> {
    agent.ok_or_else(|| {
        ToolError::Message(format!(
            "{tool_name} is unavailable because this run has no active multi-agent context"
        ))
    })
}

fn require_message(tool_name: &str, message: &str) -> Result<(), ToolError> {
    if message.trim().is_empty() {
        return Err(ToolError::Message(format!(
            "{tool_name} requires a non-empty `message`"
        )));
    }
    Ok(())
}

fn parse_fork_turns(value: Option<&Value>) -> Result<AgentForkTurns, ToolError> {
    let Some(value) = value else {
        return Ok(AgentForkTurns::All);
    };
    let Some(value) = value.as_str() else {
        return Err(ToolError::Message(
            "spawn_agent `fork_turns` must be the string `none` or `all`".to_string(),
        ));
    };
    match value.trim() {
        "none" => Ok(AgentForkTurns::None),
        "all" => Ok(AgentForkTurns::All),
        value if value.parse::<usize>().is_ok_and(|turns| turns > 0) => {
            Err(ToolError::Message(
                "spawn_agent `fork_turns` positive turn counts are not supported by this moyAI multi-agent version; use `none` or `all`"
                    .to_string(),
            ))
        }
        _ => Err(ToolError::Message(
            "spawn_agent `fork_turns` must be `none` or `all`".to_string(),
        )),
    }
}

fn validated_timeout(timeout_ms: Option<u64>) -> Result<u64, ToolError> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_WAIT_TIMEOUT_MS);
    if timeout_ms < MIN_WAIT_TIMEOUT_MS {
        return Err(ToolError::Message(format!(
            "wait_agent `timeout_ms` must be at least {MIN_WAIT_TIMEOUT_MS}"
        )));
    }
    if timeout_ms > MAX_WAIT_TIMEOUT_MS {
        return Err(ToolError::Message(format!(
            "wait_agent `timeout_ms` must be at most {MAX_WAIT_TIMEOUT_MS}"
        )));
    }
    Ok(timeout_ms)
}

fn json_result(title: &str, output: Value, metadata: Value) -> Result<ToolResult, ToolError> {
    Ok(ToolResult {
        title: title.to_string(),
        output_text: serde_json::to_string(&output)?,
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_schema_matches_the_bounded_v2_surface() {
        let spec = SpawnAgentTool.spec();
        assert_eq!(spec.name, ToolName::SpawnAgent);
        assert_eq!(
            spec.input_schema["required"],
            json!(["task_name", "message"])
        );
        assert_eq!(
            spec.input_schema["properties"]["fork_turns"]["enum"],
            json!(["none", "all"])
        );
        assert!(spec.input_schema["properties"].get("agent_type").is_none());
        assert!(spec.input_schema["properties"].get("model").is_none());
    }

    #[test]
    fn fork_turns_defaults_to_all_and_rejects_partial_counts() {
        assert!(matches!(
            parse_fork_turns(None).expect("default"),
            AgentForkTurns::All
        ));
        assert!(matches!(
            parse_fork_turns(Some(&json!("none"))).expect("none"),
            AgentForkTurns::None
        ));
        let error = parse_fork_turns(Some(&json!("3"))).expect_err("partial fork");
        assert!(
            error
                .to_string()
                .contains("positive turn counts are not supported")
        );
    }

    #[test]
    fn wait_timeout_has_codex_v2_bounds_and_default() {
        assert_eq!(validated_timeout(None).expect("default"), 30_000);
        assert_eq!(validated_timeout(Some(10_000)).expect("minimum"), 10_000);
        assert_eq!(
            validated_timeout(Some(3_600_000)).expect("maximum"),
            3_600_000
        );
        assert!(validated_timeout(Some(9_999)).is_err());
        assert!(validated_timeout(Some(3_600_001)).is_err());
    }

    #[test]
    fn communication_tools_reject_blank_messages() {
        assert!(require_message("send_message", "").is_err());
        assert!(require_message("followup_task", " \r\n\t").is_err());
        assert!(require_message("send_message", "message").is_ok());
    }
}
