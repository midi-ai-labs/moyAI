use crate::llm::ModelMessage;
use crate::session::{ThreadGoal, ThreadGoalStatus};

/// Request-steering state captured once at turn admission. Usage accounting may
/// continue durably, but those counters cannot mutate the provider-visible
/// contract between tool rounds in the same turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalSnapshot {
    goal_id: String,
    objective: String,
    status: ThreadGoalStatus,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
}

impl GoalSnapshot {
    pub(crate) fn capture(goal_id: impl Into<String>, goal: &ThreadGoal) -> Self {
        Self {
            goal_id: goal_id.into(),
            objective: goal.objective.clone(),
            status: goal.status,
            token_budget: goal.token_budget,
            tokens_used: goal.tokens_used,
            time_used_seconds: goal.time_used_seconds,
        }
    }

    pub(super) fn goal_id(&self) -> &str {
        &self.goal_id
    }
}

const CONTINUATION_PROMPT_TEMPLATE: &str =
    include_str!("../../assets/prompts/goals/continuation.md");
const BUDGET_LIMIT_PROMPT_TEMPLATE: &str =
    include_str!("../../assets/prompts/goals/budget_limit.md");

pub(super) fn steering_message_for_goal(goal: &GoalSnapshot) -> Option<ModelMessage> {
    let prompt = match goal.status {
        ThreadGoalStatus::Active => continuation_prompt(goal),
        ThreadGoalStatus::BudgetLimited => budget_limit_prompt(goal),
        ThreadGoalStatus::Paused
        | ThreadGoalStatus::Blocked
        | ThreadGoalStatus::UsageLimited
        | ThreadGoalStatus::Complete => return None,
    };
    Some(ModelMessage::User { content: prompt })
}

fn continuation_prompt(goal: &GoalSnapshot) -> String {
    let objective = escape_xml_text(&goal.objective);
    let tokens_used = goal.tokens_used.to_string();
    let token_budget = token_budget_text(goal);
    let remaining_tokens = goal
        .token_budget
        .map(|budget| (budget - goal.tokens_used).max(0).to_string())
        .unwrap_or_else(|| "unbounded".to_string());

    render_template(
        CONTINUATION_PROMPT_TEMPLATE,
        &[
            ("objective", objective.as_str()),
            ("tokens_used", tokens_used.as_str()),
            ("token_budget", token_budget.as_str()),
            ("remaining_tokens", remaining_tokens.as_str()),
        ],
    )
}

fn budget_limit_prompt(goal: &GoalSnapshot) -> String {
    let objective = escape_xml_text(&goal.objective);
    let time_used_seconds = goal.time_used_seconds.to_string();
    let tokens_used = goal.tokens_used.to_string();
    let token_budget = token_budget_text(goal);

    render_template(
        BUDGET_LIMIT_PROMPT_TEMPLATE,
        &[
            ("objective", objective.as_str()),
            ("time_used_seconds", time_used_seconds.as_str()),
            ("tokens_used", tokens_used.as_str()),
            ("token_budget", token_budget.as_str()),
        ],
    )
}

fn token_budget_text(goal: &GoalSnapshot) -> String {
    goal.token_budget
        .map(|budget| budget.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn render_template(template: &str, values: &[(&str, &str)]) -> String {
    let mut rendered = template.to_string();
    for (name, value) in values {
        rendered = rendered.replace(&format!("{{{{ {name} }}}}"), value);
    }
    rendered
}

fn escape_xml_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    fn goal(status: ThreadGoalStatus) -> ThreadGoal {
        ThreadGoal {
            thread_id: SessionId::new(),
            objective: "fix <all> & verify".to_string(),
            status,
            token_budget: Some(100),
            tokens_used: 40,
            time_used_seconds: 7,
            created_at: 1,
            updated_at: 2,
        }
    }

    #[test]
    fn active_goal_uses_continuation_template() {
        let Some(ModelMessage::User { content }) = steering_message_for_goal(
            &GoalSnapshot::capture("goal-1", &goal(ThreadGoalStatus::Active)),
        ) else {
            panic!("active goal should produce steering");
        };

        assert!(content.contains("Continue working toward the active thread goal."));
        assert!(content.contains("fix &lt;all&gt; &amp; verify"));
        assert!(content.contains("- Tokens remaining: 60"));
        assert!(!content.contains("{{"));
    }

    #[test]
    fn budget_limited_goal_uses_budget_template() {
        let Some(ModelMessage::User { content }) = steering_message_for_goal(
            &GoalSnapshot::capture("goal-1", &goal(ThreadGoalStatus::BudgetLimited)),
        ) else {
            panic!("budget limited goal should produce steering");
        };

        assert!(content.contains("has reached its token budget"));
        assert!(content.contains("- Time spent pursuing goal: 7 seconds"));
        assert!(!content.contains("{{"));
    }

    #[test]
    fn terminal_goal_does_not_produce_steering() {
        assert!(
            steering_message_for_goal(&GoalSnapshot::capture(
                "goal-1",
                &goal(ThreadGoalStatus::Complete),
            ))
            .is_none()
        );
    }
}
