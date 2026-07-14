use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::session::{
    SessionId, SessionRepository, TodoId, TodoItem, TodoKind, TodoPriority, TodoStatus,
};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Clone, Deserialize)]
pub struct TodoWriteInput {
    pub todos: Vec<TodoWriteInputItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePlanInput {
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<UpdatePlanInputItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePlanInputItem {
    pub step: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TodoWriteInputItem {
    #[serde(default)]
    pub id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub kind: Option<TodoKind>,
    pub status: TodoStatus,
    #[serde(default)]
    pub priority: Option<TodoPriority>,
    #[serde(default, deserialize_with = "deserialize_utf8_path_vec")]
    pub targets: Vec<Utf8PathBuf>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub depends_on: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub success_criteria: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Default)]
pub struct UpdatePlanTool;

fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    string_vec_from_value(value).map_err(serde::de::Error::custom)
}

fn deserialize_utf8_path_vec<'de, D>(deserializer: D) -> Result<Vec<Utf8PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = deserialize_string_vec(deserializer)?;
    Ok(values.into_iter().map(Utf8PathBuf::from).collect())
}

fn string_vec_from_value(value: Value) -> Result<Vec<String>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![trimmed.to_string()])
            }
        }
        Value::Array(items) => {
            let mut values = Vec::new();
            for item in items {
                match item {
                    Value::String(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            values.push(trimmed.to_string());
                        }
                    }
                    Value::Null => {}
                    other => {
                        return Err(format!(
                            "expected string entries in todo array field, got {other}"
                        ));
                    }
                }
            }
            Ok(values)
        }
        other => Err(format!(
            "expected a string or array of strings for todo field, got {other}"
        )),
    }
}

#[async_trait(?Send)]
impl Tool for UpdatePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::UpdatePlan,
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
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let session_repo = ctx.services.store.session_repo();
        let existing_todos = session_repo.list_todos(ctx.session.session.id).await?;
        let (todos, explanation) =
            effective_plan_from_arguments(ctx.session.session.id, raw_arguments, &existing_todos)?;
        validate_todos(&todos)?;
        ctx.run_mutation_fence.assert_owned().await?;
        session_repo
            .update_todos(ctx.session.session.id, &todos)
            .await?;

        let open_count = todos.iter().filter(|todo| todo.status.is_open()).count();
        let blocked_count = todos
            .iter()
            .filter(|todo| matches!(todo.status, TodoStatus::Blocked))
            .count();

        Ok(ToolResult {
            title: "Plan updated".to_string(),
            output_text: "Plan updated".to_string(),
            metadata: json!({
                "progress_projection": true,
                "open_count": open_count,
                "blocked_count": blocked_count,
                "todo_count": todos.len(),
                "explanation": explanation,
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

pub(crate) fn effective_plan_from_arguments(
    session_id: SessionId,
    raw_arguments: Value,
    existing_todos: &[TodoItem],
) -> Result<(Vec<TodoItem>, Option<String>), ToolError> {
    let normalized = match raw_arguments {
        Value::String(text) => serde_json::from_str::<Value>(&text).map_err(|error| {
            ToolError::Message(format!(
                "update_plan arguments must be valid JSON when sent as a string: {error}"
            ))
        })?,
        other => other,
    };

    // Accept the former rich `todowrite` payload only as a compatibility reader. The
    // advertised schema and all newly persisted tool identities use `update_plan`.
    if normalized.get("plan").is_none() && normalized.get("todos").is_some() {
        return effective_todos_from_arguments(session_id, normalized, existing_todos)
            .map(|todos| (todos, None));
    }

    let input = serde_json::from_value::<UpdatePlanInput>(normalized)?;
    let explanation = input
        .explanation
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let todos = input
        .plan
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let content = item.step.trim().to_string();
            if content.is_empty() {
                return Err(ToolError::Message(
                    "update_plan requires every step to be non-empty".to_string(),
                ));
            }
            if matches!(item.status, TodoStatus::Blocked | TodoStatus::Cancelled) {
                return Err(ToolError::Message(
                    "update_plan status must be `pending`, `in_progress`, or `completed`"
                        .to_string(),
                ));
            }
            Ok(TodoItem {
                id: TodoId::from_stable_input(&format!(
                    "{session_id}:update_plan:{index}:{content}"
                )),
                content,
                kind: TodoKind::Work,
                status: item.status,
                priority: if matches!(item.status, TodoStatus::InProgress) {
                    TodoPriority::High
                } else {
                    TodoPriority::Medium
                },
                targets: Vec::new(),
                depends_on: Vec::new(),
                success_criteria: Vec::new(),
                blocked_by: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((todos, explanation))
}

pub(crate) fn effective_todos_from_arguments(
    session_id: SessionId,
    raw_arguments: Value,
    existing_todos: &[TodoItem],
) -> Result<Vec<TodoItem>, ToolError> {
    let _ = existing_todos;
    let input =
        serde_json::from_value::<TodoWriteInput>(normalize_todo_write_arguments(raw_arguments)?)?;
    normalize_todos(session_id, input.todos)
}

pub(crate) fn normalize_todo_write_arguments(raw_arguments: Value) -> Result<Value, ToolError> {
    let mut normalized = match raw_arguments {
        Value::String(text) => serde_json::from_str::<Value>(&text).map_err(|error| {
            ToolError::Message(format!(
                "todowrite arguments must be valid JSON when sent as a string: {error}"
            ))
        })?,
        other => other,
    };

    let Some(todos) = normalized.get_mut("todos") else {
        return Ok(normalized);
    };

    if let Some(text) = todos.as_str() {
        let parsed = serde_json::from_str::<Value>(text).map_err(|error| {
            ToolError::Message(format!(
                "todowrite `todos` must be a real JSON array, not an invalid string payload: {error}"
            ))
        })?;
        let Value::Array(entries) = parsed else {
            return Err(ToolError::Message(
                "todowrite `todos` must decode to a JSON array when sent as a string".to_string(),
            ));
        };
        *todos = Value::Array(entries);
    }

    Ok(normalized)
}

fn normalize_todos(
    session_id: crate::session::SessionId,
    input: Vec<TodoWriteInputItem>,
) -> Result<Vec<TodoItem>, ToolError> {
    let normalized = input
        .into_iter()
        .enumerate()
        .map(|(index, todo)| {
            let content = todo.content.trim().to_string();
            let raw_id = todo.id.clone().unwrap_or_else(|| {
                format!(
                    "__generated__:{index}:{}:{}",
                    content,
                    todo_status_text(todo.status)
                )
            });
            (raw_id, content, todo)
        })
        .collect::<Vec<_>>();
    let id_map = normalized
        .iter()
        .map(|(raw_id, _, _)| (raw_id.clone(), resolve_supplied_todo_id(session_id, raw_id)))
        .collect::<HashMap<_, _>>();

    normalized
        .into_iter()
        .map(|(raw_id, content, todo)| {
            let kind = todo.kind.unwrap_or(TodoKind::Work);
            let priority = todo
                .priority
                .unwrap_or_else(|| infer_todo_priority(kind, todo.status));
            let depends_on = todo
                .depends_on
                .into_iter()
                .map(|dependency| {
                    let dependency = dependency.trim();
                    id_map.get(dependency).copied().ok_or_else(|| {
                        ToolError::Message(format!(
                            "todowrite dependency `{dependency}` does not reference an id in the submitted checklist"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TodoItem {
                id: *id_map
                    .get(&raw_id)
                    .expect("normalized todo id should exist in id_map"),
                content,
                kind,
                status: todo.status,
                priority,
                targets: dedup_paths(todo.targets),
                depends_on,
                success_criteria: dedup_trimmed(todo.success_criteria),
                blocked_by: dedup_trimmed(todo.blocked_by),
            })
        })
        .collect()
}

fn resolve_supplied_todo_id(session_id: crate::session::SessionId, raw_id: &str) -> TodoId {
    let trimmed = raw_id.trim();
    TodoId::from_str(trimmed)
        .unwrap_or_else(|_| TodoId::from_stable_input(&format!("{session_id}:todo:{trimmed}")))
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), ToolError> {
    let in_progress = todos
        .iter()
        .filter(|todo| matches!(todo.status, TodoStatus::InProgress))
        .count();
    if in_progress > 1 {
        return Err(ToolError::Message(
            "todowrite accepts at most one `in_progress` item".to_string(),
        ));
    }

    if todos.iter().any(|todo| todo.content.trim().is_empty()) {
        return Err(ToolError::Message(
            "todowrite requires every todo item to have non-empty `content`".to_string(),
        ));
    }

    let ids = todos.iter().map(|todo| todo.id).collect::<HashSet<_>>();
    if ids.len() != todos.len() {
        return Err(ToolError::Message(
            "todowrite requires every todo item to have a unique `id`".to_string(),
        ));
    }

    for todo in todos {
        if matches!(todo.status, TodoStatus::Blocked) && todo.blocked_by.is_empty() {
            return Err(ToolError::Message(
                "todowrite requires blocked items to include at least one `blocked_by` reason"
                    .to_string(),
            ));
        }
    }

    Ok(())
}

fn infer_todo_priority(kind: TodoKind, status: TodoStatus) -> TodoPriority {
    if !matches!(kind, TodoKind::Work) {
        return TodoPriority::High;
    }
    match status {
        TodoStatus::InProgress => TodoPriority::High,
        TodoStatus::Pending | TodoStatus::Blocked => TodoPriority::Medium,
        TodoStatus::Completed | TodoStatus::Cancelled => TodoPriority::Medium,
    }
}

fn dedup_paths(values: Vec<Utf8PathBuf>) -> Vec<Utf8PathBuf> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        if value.as_str().trim().is_empty() {
            continue;
        }
        if seen.insert(value.clone()) {
            result.push(value);
        }
    }
    result
}

fn dedup_trimmed(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            result.push(trimmed.to_string());
        }
    }
    result
}

fn todo_status_text(value: TodoStatus) -> &'static str {
    match value {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Blocked => "blocked",
        TodoStatus::Completed => "completed",
        TodoStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_plan_exposes_the_small_codex_style_schema() {
        let spec = UpdatePlanTool.spec();

        assert_eq!(spec.name, ToolName::UpdatePlan);
        assert_eq!(spec.input_schema["required"], json!(["plan"]));
        assert_eq!(
            spec.input_schema["properties"]["plan"]["items"]["required"],
            json!(["step", "status"])
        );
        assert!(spec.input_schema["properties"].get("todos").is_none());
    }

    #[test]
    fn update_plan_normalizes_only_projection_fields() {
        let session_id = SessionId::new();
        let (todos, explanation) = effective_plan_from_arguments(
            session_id,
            json!({
                "explanation": "Evidence changed the next step",
                "plan": [
                    {"step": "Inspect the relevant contracts", "status": "completed"},
                    {"step": "Implement the smallest coherent change", "status": "in_progress"},
                    {"step": "Verify the outcome", "status": "pending"}
                ]
            }),
            &[],
        )
        .expect("plan");

        assert_eq!(
            explanation.as_deref(),
            Some("Evidence changed the next step")
        );
        assert_eq!(todos.len(), 3);
        assert!(todos.iter().all(|todo| todo.kind == TodoKind::Work));
        assert!(todos.iter().all(|todo| todo.targets.is_empty()));
        assert!(todos.iter().all(|todo| todo.depends_on.is_empty()));
        assert_eq!(todos[1].priority, TodoPriority::High);
        validate_todos(&todos).expect("valid plan");
    }

    #[test]
    fn update_plan_rejects_legacy_only_statuses_in_the_canonical_shape() {
        let error = effective_plan_from_arguments(
            SessionId::new(),
            json!({
                "plan": [{"step": "Wait for input", "status": "blocked"}]
            }),
            &[],
        )
        .expect_err("blocked is not canonical");

        assert!(error.to_string().contains("pending"));
    }

    #[test]
    fn todo_write_preserves_declared_contract_fields() {
        let session_id = SessionId::new();
        let todos = effective_todos_from_arguments(
            session_id,
            json!({
                "todos": [
                    {
                        "id": "implement",
                        "content": "Implement the fix",
                        "kind": "work",
                        "status": "completed",
                        "targets": ["src/lib.rs"],
                        "success_criteria": ["build passes"]
                    },
                    {
                        "id": "verify",
                        "content": "Verify the fix",
                        "kind": "verification",
                        "status": "in_progress",
                        "depends_on": ["implement"],
                        "success_criteria": ["tests pass"]
                    }
                ]
            }),
            &[],
        )
        .expect("todos");

        assert_eq!(todos[0].targets, vec![Utf8PathBuf::from("src/lib.rs")]);
        assert_eq!(todos[0].success_criteria, vec!["build passes"]);
        assert_eq!(todos[1].kind, TodoKind::Verification);
        assert_eq!(todos[1].depends_on, vec![todos[0].id]);
        assert_eq!(todos[1].priority, TodoPriority::High);
    }

    #[test]
    fn todo_write_rejects_unknown_dependency_ids() {
        let error = effective_todos_from_arguments(
            SessionId::new(),
            json!({
                "todos": [{
                    "id": "verify",
                    "content": "Verify",
                    "status": "pending",
                    "depends_on": ["missing"]
                }]
            }),
            &[],
        )
        .expect_err("unknown dependency");

        assert!(error.to_string().contains("does not reference an id"));
    }
}
