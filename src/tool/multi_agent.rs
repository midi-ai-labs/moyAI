use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

use crate::app::{AgentForkTurns, AgentRunContext};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
const MIN_WAIT_TIMEOUT_MS: u64 = 10_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 3_600_000;
const DURABLE_STEER_POLL_INTERVAL_MS: u64 = 100;

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
            description: r#"Spawns an agent to work on the specified task. If your current task is `/root/task1` and you spawn_agent with task_name "task_3" the agent will have canonical task name `/root/task1/task_3`.
You are then able to refer to this agent as `task_3` or `/root/task1/task_3` interchangeably. However an agent `/root/task2/task_3` would only be able to communicate with this agent via its canonical name `/root/task1/task_3`.
The spawned agent will have the same tools as you and the ability to spawn its own subagents.

Only call this tool for a concrete, bounded subtask that can run independently alongside useful local work; otherwise continue locally.
It will be able to send you and other running agents messages, and its final answer will be provided to you when it finishes.
The new agent's canonical task name will be provided to it along with the message.

Note that passing `fork_turns="none"` will not pass any surrounding context to the spawned subagent, which may cause the agent to lack the context it needs to complete its task, whereas `fork_turns="all"` will provide the subagent with all surrounding context."#,
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
                        "description": "Optional number of turns to fork. Defaults to `all`. Use `none`, `all`, or a positive integer string such as `3` to fork only the most recent turns."
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
            description: "Wait for a mailbox update from any live agent. The wait also ends early when new user input is steered into the active turn. Omit timeout_ms for normal delegated work so the activity-sensitive default avoids short polling. Returns only an activity, interruption, or timeout summary, never hidden reasoning or message content.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": MIN_WAIT_TIMEOUT_MS,
                        "maximum": MAX_WAIT_TIMEOUT_MS,
                        "description": "Timeout in milliseconds. Defaults to 30000, min 10000, max 3600000. The wait returns early when agent activity arrives."
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
        let result = wait_for_agent_activity_or_steer_with_poll_interval(
            agent,
            &active_runs,
            session_id,
            timeout_ms,
            Duration::from_millis(DURABLE_STEER_POLL_INTERVAL_MS),
        )
        .await?;
        let output = serde_json::to_value(&result)?;
        json_result("Agent wait completed", output.clone(), output)
    }
}

pub(crate) async fn wait_for_agent_activity_or_steer_with_poll_interval(
    agent: &AgentRunContext,
    active_runs: &crate::runtime::ActiveRunRegistry,
    session_id: crate::session::SessionId,
    timeout_ms: u64,
    durable_poll_interval: Duration,
) -> Result<crate::app::AgentWaitResult, ToolError> {
    let steer_generation = active_runs
        .steer_generation(session_id)
        .map_err(|error| ToolError::Message(error.to_string()))?;
    // The process-local generation is only a wakeup edge. The durable queue
    // remains the content owner, so check it after capturing the generation
    // and before waiting. A commit before the capture is observed here; a
    // same-process commit after this check changes the generation and is
    // observed by wait_for_steer_activity when it subscribes. The bounded
    // durable poll covers another StoreBundle/process.
    if agent
        .has_pending_turn_steer_input()
        .map_err(ToolError::Message)?
    {
        return Ok(steered_wait_result());
    }
    let result = tokio::select! {
        result = agent.wait_for_activity(timeout_ms) => {
            result.map_err(ToolError::Message)?
        }
        steer = active_runs.wait_for_steer_activity(session_id, steer_generation) => {
            steer.map_err(|error| ToolError::Message(error.to_string()))?;
            steered_wait_result()
        }
        durable_steer = wait_for_durable_turn_steer(agent, durable_poll_interval) => {
            durable_steer.map_err(ToolError::Message)?;
            steered_wait_result()
        }
    };
    // A cross-process commit can land after the last bounded poll but before
    // the timeout branch wins. Recheck the durable owner before accepting only
    // a timeout; real mailbox activity returned above remains authoritative.
    if result.timed_out
        && agent
            .has_pending_turn_steer_input()
            .map_err(ToolError::Message)?
    {
        return Ok(steered_wait_result());
    }
    Ok(result)
}

fn steered_wait_result() -> crate::app::AgentWaitResult {
    crate::app::AgentWaitResult {
        message: "Wait interrupted by new user input.".to_string(),
        timed_out: false,
        updated_agents: Vec::new(),
    }
}

async fn wait_for_durable_turn_steer(
    agent: &AgentRunContext,
    durable_poll_interval: Duration,
) -> Result<(), String> {
    loop {
        tokio::time::sleep(durable_poll_interval).await;
        if agent.has_pending_turn_steer_input()? {
            return Ok(());
        }
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
        let effect_commit = ctx.run_mutation_fence.begin_effect_commit()?;
        let agent = require_agent_context("interrupt_agent", ctx.agent)?;
        let interrupted = agent.interrupt_agent(&input.target, activity_id.clone());
        effect_commit.release();
        let (agent_path, status) = interrupted.map_err(ToolError::Message)?;
        let output = json!({
            "agent_path": agent_path,
            "status": status,
        });
        let metadata = json!({
            "activity_id": activity_id,
            "agent_path": agent_path,
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
    let effect_commit = ctx.run_mutation_fence.begin_effect_commit()?;
    let agent = require_agent_context(tool_name, ctx.agent)?;
    let delivery = agent
        .send_message(
            &input.target,
            input.message,
            trigger_turn,
            activity_id.clone(),
        )
        .await;
    effect_commit.release();
    let agent_path = delivery.map_err(ToolError::Message)?;
    let output = json!({
        "agent_path": agent_path,
        "queued": true,
        "trigger_turn": trigger_turn,
    });
    let metadata = json!({
        "activity_id": activity_id,
        "agent_path": agent_path,
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
            "spawn_agent `fork_turns` must be `none`, `all`, or a positive integer string"
                .to_string(),
        ));
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(AgentForkTurns::All);
    }
    match value.to_ascii_lowercase().as_str() {
        "none" => Ok(AgentForkTurns::None),
        "all" => Ok(AgentForkTurns::All),
        value if value.parse::<usize>().is_ok_and(|turns| turns > 0) => Ok(AgentForkTurns::Recent(
            value.parse::<usize>().expect("validated positive integer"),
        )),
        _ => Err(ToolError::Message(
            "spawn_agent `fork_turns` must be `none`, `all`, or a positive integer string"
                .to_string(),
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
        _internal_file_lease: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_schema_matches_the_codex_fork_turns_surface() {
        let spec = SpawnAgentTool.spec();
        assert_eq!(spec.name, ToolName::SpawnAgent);
        assert_eq!(
            spec.input_schema["required"],
            json!(["task_name", "message"])
        );
        let fork_turns = &spec.input_schema["properties"]["fork_turns"];
        assert_eq!(fork_turns["type"], json!("string"));
        assert!(fork_turns.get("enum").is_none());
        let fork_turns_description = fork_turns["description"]
            .as_str()
            .expect("fork_turns description");
        assert!(fork_turns_description.contains("`none`"));
        assert!(fork_turns_description.contains("`all`"));
        assert!(fork_turns_description.contains("positive integer string"));
        assert!(spec.description.contains("same tools"));
        assert!(spec.description.contains("spawn its own subagents"));
        assert!(spec.description.contains("fork_turns=\"none\""));
        assert!(spec.description.contains("fork_turns=\"all\""));
        assert!(spec.input_schema["properties"].get("agent_type").is_none());
        assert!(spec.input_schema["properties"].get("model").is_none());
    }

    #[test]
    fn fork_turns_defaults_to_all_and_accepts_codex_string_values() {
        assert_eq!(
            parse_fork_turns(None).expect("default"),
            AgentForkTurns::All
        );
        assert_eq!(
            parse_fork_turns(Some(&json!(" NoNe "))).expect("none"),
            AgentForkTurns::None
        );
        assert_eq!(
            parse_fork_turns(Some(&json!("ALL"))).expect("all"),
            AgentForkTurns::All
        );
        assert_eq!(
            parse_fork_turns(Some(&json!("  "))).expect("blank defaults to all"),
            AgentForkTurns::All
        );
        assert_eq!(
            parse_fork_turns(Some(&json!("3"))).expect("recent turns"),
            AgentForkTurns::Recent(3)
        );
        for invalid in [json!("0"), json!("-1"), json!("1.5"), json!(3)] {
            let error = parse_fork_turns(Some(&invalid)).expect_err("invalid fork_turns");
            assert!(error.to_string().contains("positive integer string"));
        }
    }

    #[test]
    fn wait_timeout_matches_codex_v2_default_and_bounds() {
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
