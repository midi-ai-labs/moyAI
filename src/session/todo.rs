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
    matches!(todo.kind, TodoKind::Completion) || is_completion_todo(&todo.content)
}

pub fn todo_counts_as_open_work(todo: &TodoItem) -> bool {
    todo.status.is_open() && !todo_is_completion_item(todo)
}

pub fn is_completion_todo(content: &str) -> bool {
    let lower = content.trim().to_ascii_lowercase();
    let has_english_completion = lower.contains("close out")
        || lower.contains("close-out")
        || lower.contains("final check")
        || lower.contains("final review")
        || lower.contains("final verification")
        || lower.contains("wrap up")
        || lower.contains("finish the run")
        || lower.contains("finalize");
    let has_japanese_completion = (content.contains("完了")
        || content.contains("終了")
        || content.contains("最終")
        || content.contains("クローズ"))
        && (content.contains("確認")
            || content.contains("整合")
            || content.contains("照合")
            || content.contains("再読")
            || content.contains("チェック")
            || content.contains("完了条件"))
        || content.contains("整合確認")
        || content.contains("整合性確認")
        || content.contains("完了条件チェック");
    let has_japanese_reread_closeout = (content.contains("再読")
        || content.contains("読み直し")
        || content.contains("読み直す"))
        && (content.contains("確認") || content.contains("照合") || content.contains("チェック"));
    let has_unsupported_claim_closeout = lower.contains("unsupported claim")
        && (lower.contains("reread")
            || lower.contains("read again")
            || lower.contains("final review")
            || lower.contains("final check")
            || content.contains("再読")
            || content.contains("読み直し")
            || content.contains("読み直す"));

    has_english_completion
        || has_japanese_completion
        || has_japanese_reread_closeout
        || has_unsupported_claim_closeout
}
