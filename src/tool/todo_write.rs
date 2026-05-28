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
pub struct TodoWriteTool;

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
impl Tool for TodoWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TodoWrite,
            description: "Update the client-visible progress checklist for the current run. This is a progress projection like Codex `update_plan`; it does not replace requested-work, verification, closeout, or tool authority.",
            input_schema: json!({
                "type": "object",
                "required": ["todos"],
                "properties": {
                    "todos": {
                        "type": "array",
                            "description": "The complete updated progress checklist for the current run.",
                        "items": {
                            "type": "object",
                            "required": ["content", "status"],
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Stable todo id. Human-readable ids such as `step1` are allowed; omitted ids are normalized automatically."
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Short task description."
                                },
                                "kind": {
                                    "type": "string",
                                    "enum": ["work", "verification", "repair", "completion"],
                                    "description": "Task kind. Optional when the task is plain work."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "blocked", "completed", "cancelled"]
                                },
                                "priority": {
                                    "type": "string",
                                    "enum": ["high", "medium", "low"],
                                    "description": "Optional priority. If omitted, moyai defaults verification/repair/completion or in-progress work to `high`, and defaults the rest to `medium`."
                                },
                                "targets": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Primary files or directories touched by this task."
                                },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Todo ids that must be completed before this task becomes actionable. These may reference the same human-readable ids used in this payload."
                                },
                                "success_criteria": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Short acceptance criteria for this task."
                                },
                                "blocked_by": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Concrete reasons this blocked task cannot proceed yet."
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
        let mut todos =
            effective_todos_from_arguments(ctx.session.session.id, raw_arguments, &existing_todos)?;
        normalize_progress_projection_todos(&mut todos);
        validate_todos(&todos)?;
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
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

pub(crate) fn effective_todos_from_arguments(
    session_id: SessionId,
    raw_arguments: Value,
    existing_todos: &[TodoItem],
) -> Result<Vec<TodoItem>, ToolError> {
    let _ = existing_todos;
    let input =
        serde_json::from_value::<TodoWriteInput>(normalize_todo_write_arguments(raw_arguments)?)?;
    Ok(normalize_todos(session_id, input.todos))
}

pub(crate) fn progress_projection_payload_drops_authority_fields() -> bool {
    let raw = json!({
        "todos": [{
            "id": "step1",
            "content": "write component.py",
            "kind": "verification",
            "status": "in_progress",
            "targets": ["component.py"],
            "depends_on": ["prior"],
            "success_criteria": ["python -m unittest"]
        }]
    });
    let mut todos = match effective_todos_from_arguments(SessionId::new(), raw, &[]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    normalize_progress_projection_todos(&mut todos);
    todos.len() == 1
        && matches!(todos[0].kind, TodoKind::Work)
        && todos[0].targets.is_empty()
        && todos[0].depends_on.is_empty()
        && todos[0].success_criteria.is_empty()
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

fn normalize_progress_projection_todos(todos: &mut [TodoItem]) {
    for todo in todos {
        todo.kind = TodoKind::Work;
        todo.targets.clear();
        todo.depends_on.clear();
        todo.success_criteria.clear();
    }
}

fn normalize_todos(
    session_id: crate::session::SessionId,
    input: Vec<TodoWriteInputItem>,
) -> Vec<TodoItem> {
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

    let todos = normalized
        .into_iter()
        .map(|(raw_id, content, todo)| {
            let kind = TodoKind::Work;
            let priority = todo
                .priority
                .unwrap_or_else(|| infer_todo_priority(todo.status));
            TodoItem {
                id: *id_map
                    .get(&raw_id)
                    .expect("normalized todo id should exist in id_map"),
                content,
                kind,
                status: todo.status,
                priority,
                targets: Vec::new(),
                depends_on: Vec::new(),
                success_criteria: Vec::new(),
                blocked_by: dedup_trimmed(todo.blocked_by),
            }
        })
        .collect::<Vec<_>>();

    todos
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

fn infer_todo_priority(status: TodoStatus) -> TodoPriority {
    match status {
        TodoStatus::InProgress => TodoPriority::High,
        TodoStatus::Pending | TodoStatus::Blocked => TodoPriority::Medium,
        TodoStatus::Completed | TodoStatus::Cancelled => TodoPriority::Medium,
    }
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
