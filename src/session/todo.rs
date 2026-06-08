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

pub(crate) fn todo_completion_kind_only_open_work_authority_fixture_passes() -> bool {
    let work_with_closeout_words = TodoItem {
        id: TodoId::new(),
        content: "final verification and close out remaining workflow evidence".to_string(),
        kind: TodoKind::Work,
        status: TodoStatus::Pending,
        priority: TodoPriority::High,
        targets: Vec::new(),
        depends_on: Vec::new(),
        success_criteria: Vec::new(),
        blocked_by: Vec::new(),
    };
    let typed_completion = TodoItem {
        id: TodoId::new(),
        content: "continue implementation".to_string(),
        kind: TodoKind::Completion,
        status: TodoStatus::Pending,
        priority: TodoPriority::Low,
        targets: Vec::new(),
        depends_on: Vec::new(),
        success_criteria: Vec::new(),
        blocked_by: Vec::new(),
    };

    !todo_is_completion_item(&work_with_closeout_words)
        && todo_counts_as_open_work(&work_with_closeout_words)
        && todo_is_completion_item(&typed_completion)
        && !todo_counts_as_open_work(&typed_completion)
}

#[cfg(test)]
mod tests {
    #[test]
    fn todo_completion_kind_only_open_work_authority() {
        assert!(super::todo_completion_kind_only_open_work_authority_fixture_passes());
    }
}
