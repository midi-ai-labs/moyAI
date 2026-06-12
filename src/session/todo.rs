use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use super::TodoId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Blocked,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub fn is_open(self) -> bool {
        !matches!(self, TodoStatus::Completed | TodoStatus::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoKind {
    #[default]
    Work,
    Verification,
    Repair,
    Completion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: TodoId,
    pub content: String,
    pub kind: TodoKind,
    pub status: TodoStatus,
    pub priority: TodoPriority,
    #[serde(default)]
    pub targets: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub depends_on: Vec<TodoId>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

impl TodoItem {
    pub fn simple(content: impl Into<String>, status: TodoStatus, priority: TodoPriority) -> Self {
        Self {
            id: TodoId::new(),
            content: content.into(),
            kind: TodoKind::Work,
            status,
            priority,
            targets: Vec::new(),
            depends_on: Vec::new(),
            success_criteria: Vec::new(),
            blocked_by: Vec::new(),
        }
    }
}

pub fn todo_is_completion_item(todo: &TodoItem) -> bool {
    matches!(todo.kind, TodoKind::Completion)
}

pub fn todo_counts_as_open_work(todo: &TodoItem) -> bool {
    todo.status.is_open() && !todo_is_completion_item(todo)
}

#[cfg(test)]
mod tests {}
