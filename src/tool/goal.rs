use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::future::Future;

use crate::error::ToolError;
use crate::session::{ThreadGoal, ThreadGoalStatus, validate_thread_goal_objective};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Default)]
pub struct GetGoalTool;

#[derive(Debug, Default)]
pub struct CreateGoalTool;

#[derive(Debug, Default)]
pub struct UpdateGoalTool;

#[derive(Debug, Deserialize)]
struct CreateGoalInput {
    objective: String,
    #[serde(default)]
    token_budget: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UpdateGoalInput {
    status: ThreadGoalStatus,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalToolResponse {
    goal: Option<ThreadGoal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_budget_report: Option<String>,
}

#[async_trait(?Send)]
impl Tool for GetGoalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::GetGoal,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Get the current goal for this thread, including status, budgets, token and elapsed usage, and remaining token budget.",
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(
        &self,
        _raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let goal = ctx
            .services
            .store
            .session_repo()
            .get_thread_goal(ctx.session.session.id)
            .await?;
        goal_tool_result("Goal read", goal, false)
    }
}

#[async_trait(?Send)]
impl Tool for CreateGoalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::CreateGoal,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Create a goal only when explicitly requested by the user or system/developer instructions; do not infer goals from ordinary tasks. Set token_budget only when an explicit token budget is requested. Fails if an unfinished goal exists; use update_goal only for status.",
            input_schema: json!({
                "type": "object",
                "required": ["objective"],
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "The concrete objective to start pursuing."
                    },
                    "token_budget": {
                        "type": "integer",
                        "description": "Optional positive token budget. Omit unless explicitly requested."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<CreateGoalInput>(raw_arguments)?;
        let objective = input.objective.trim().to_string();
        validate_thread_goal_objective(&objective).map_err(ToolError::Message)?;
        validate_token_budget(input.token_budget)?;
        let repo = ctx.services.store.session_repo();
        let goal = commit_goal_mutation(&ctx.run_mutation_fence, async {
            Ok::<_, ToolError>(
                repo.insert_thread_goal(
                    ctx.session.session.id,
                    &objective,
                    ThreadGoalStatus::Active,
                    input.token_budget,
                )
                .await?,
            )
        })
        .await?;
        let Some(goal) = goal else {
            return Err(ToolError::Message(
                "cannot create a new goal because this thread has an unfinished goal; complete the existing goal first".to_string(),
            ));
        };
        goal_tool_result("Goal created", Some(goal), false)
    }
}

#[async_trait(?Send)]
impl Tool for UpdateGoalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::UpdateGoal,
            effect: crate::tool::ToolEffectPolicy::mutation(),
            description: "Use only to mark the existing goal achieved or genuinely blocked. Set status to complete only when the objective is achieved and no required work remains. Set status to blocked only after the same blocking condition has repeated for at least three consecutive goal turns and meaningful progress requires user input or an external-state change. Pause, resume, budget-limited, and usage-limited states are controlled by user/system surfaces, not this tool.",
            input_schema: json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["complete", "blocked"]
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<UpdateGoalInput>(raw_arguments)?;
        if !matches!(
            input.status,
            ThreadGoalStatus::Complete | ThreadGoalStatus::Blocked
        ) {
            return Err(ToolError::Message(
                "update_goal accepts only `complete` or `blocked`; pause, resume, budget_limited, and usage_limited are controlled by user/system surfaces".to_string(),
            ));
        }
        let repo = ctx.services.store.session_repo();
        let goal = commit_goal_mutation(&ctx.run_mutation_fence, async {
            repo.account_thread_goal_usage(ctx.session.session.id, 0)
                .await?;
            Ok::<_, ToolError>(
                repo.update_thread_goal(ctx.session.session.id, None, Some(input.status), None)
                    .await?,
            )
        })
        .await?;
        let Some(goal) = goal else {
            return Err(ToolError::Message(
                "cannot update goal because this thread has no goal".to_string(),
            ));
        };
        goal_tool_result(
            "Goal updated",
            Some(goal),
            input.status == ThreadGoalStatus::Complete,
        )
    }
}

async fn commit_goal_mutation<T>(
    fence: &crate::tool::context::RunMutationFence,
    mutation: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    fence.assert_owned().await?;
    let effect_commit = fence.begin_effect_commit()?;
    let result = mutation.await;
    effect_commit.release();
    result
}

fn validate_token_budget(token_budget: Option<i64>) -> Result<(), ToolError> {
    if token_budget.is_some_and(|budget| budget <= 0) {
        return Err(ToolError::Message(
            "goal token budget must be positive".to_string(),
        ));
    }
    Ok(())
}

fn goal_tool_result(
    title: &str,
    goal: Option<ThreadGoal>,
    include_completion_budget_report: bool,
) -> Result<ToolResult, ToolError> {
    let remaining_tokens = goal.as_ref().and_then(|goal| {
        goal.token_budget
            .map(|budget| (budget - goal.tokens_used).max(0))
    });
    let completion_budget_report = if include_completion_budget_report {
        goal.as_ref()
            .filter(|goal| goal.status == ThreadGoalStatus::Complete)
            .and_then(completion_budget_report)
    } else {
        None
    };
    let response = GoalToolResponse {
        goal: goal.clone(),
        remaining_tokens,
        completion_budget_report: completion_budget_report.clone(),
    };
    Ok(ToolResult {
        title: title.to_string(),
        output_text: serde_json::to_string(&response)?,
        metadata: json!({
            "goal": goal,
            "remainingTokens": remaining_tokens,
            "completionBudgetReport": completion_budget_report,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    })
}

fn completion_budget_report(goal: &ThreadGoal) -> Option<String> {
    if goal.token_budget.is_none() && goal.time_used_seconds <= 0 {
        None
    } else {
        Some(
            "Goal achieved. Report final usage from this tool result's structured goal fields. If `goal.tokenBudget` is present, include token usage from `goal.tokensUsed` and `goal.tokenBudget`. If `goal.timeUsedSeconds` is greater than 0, summarize elapsed time in a concise, human-friendly form appropriate to the response language."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use tokio::sync::oneshot;

    use super::commit_goal_mutation;
    use crate::config::AccessMode;
    use crate::protocol::{TurnId, TurnInterruptionCause};
    use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl};
    use crate::session::{
        NewSession, ProjectId, ProjectRepository, SessionRepository, ThreadGoalStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
    use crate::tool::context::RunMutationFence;

    async fn goal_test_owner() -> (
        StoreBundle,
        crate::session::SessionId,
        RunMutationFence,
        RunControl,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "goal effect commit".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        let turn_id = TurnId::new();
        let repo = store.session_repo();
        let admission_id = repo
            .admit_session_turn(session.id, turn_id)
            .await
            .expect("admission")
            .expect("admitted")
            .admission_id;
        let control = RunControl::new();
        let fence = RunMutationFence::new(repo, session.id, admission_id, turn_id, control.clone());
        (store, session.id, fence, control)
    }

    #[tokio::test]
    async fn stop_during_goal_commit_is_published_only_after_the_db_mutation() {
        let (store, session_id, fence, control) = goal_test_owner().await;
        let repo = store.session_repo();
        let (started_tx, started_rx) = oneshot::channel();
        let (continue_tx, continue_rx) = oneshot::channel();
        let mutation = commit_goal_mutation(&fence, async {
            started_tx.send(()).expect("publish commit reservation");
            continue_rx.await.expect("release DB mutation");
            Ok::<_, crate::error::ToolError>(
                repo.insert_thread_goal(
                    session_id,
                    "linearized goal",
                    ThreadGoalStatus::Active,
                    None,
                )
                .await?,
            )
        });
        let stop = async {
            started_rx.await.expect("commit reservation started");

            assert!(matches!(
                control.request_cancel(RunCancellationCause::Interruption(
                    TurnInterruptionCause::UserStop
                )),
                RunCancelOutcome::Deferred(_)
            ));
            assert!(!control.is_cancelled());
            assert!(
                repo.get_thread_goal(session_id)
                    .await
                    .expect("goal read")
                    .is_none()
            );
            continue_tx.send(()).expect("continue DB mutation");
        };

        let (mutation, ()) = tokio::join!(mutation, stop);
        let inserted = mutation.expect("goal mutation").expect("inserted goal");

        assert_eq!(inserted.objective, "linearized goal");
        assert_eq!(
            repo.get_thread_goal(session_id)
                .await
                .expect("persisted goal")
                .expect("goal")
                .objective,
            "linearized goal"
        );
        assert!(control.is_cancelled());
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
    }
}
