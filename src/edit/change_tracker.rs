use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::TextDiff;

use crate::runtime::SystemClock;
use crate::session::{ChangeId, ChangeKind, ToolCallId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub id: ChangeId,
    pub tool_call_id: ToolCallId,
    pub kind: ChangeKind,
    pub path_before: Option<Utf8PathBuf>,
    pub path_after: Option<Utf8PathBuf>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub diff_text: String,
    pub summary: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub change_id: ChangeId,
    pub kind: ChangeKind,
    pub path_before: Option<Utf8PathBuf>,
    pub path_after: Option<Utf8PathBuf>,
}

impl ChangeSummary {
    pub fn summary_line(&self, workspace_root: Option<&Utf8Path>) -> String {
        match self.kind {
            ChangeKind::Add => {
                format!(
                    "Added {}",
                    render_display_path(self.path_after.as_ref(), workspace_root)
                )
            }
            ChangeKind::Update => {
                format!(
                    "Updated {}",
                    render_display_path(
                        self.path_after.as_ref().or(self.path_before.as_ref()),
                        workspace_root,
                    )
                )
            }
            ChangeKind::Delete => {
                format!(
                    "Deleted {}",
                    render_display_path(self.path_before.as_ref(), workspace_root)
                )
            }
            ChangeKind::Move => format!(
                "Moved {} -> {}",
                render_display_path(self.path_before.as_ref(), workspace_root),
                render_display_path(self.path_after.as_ref(), workspace_root)
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChangeTracker;

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Add => "add",
            ChangeKind::Update => "update",
            ChangeKind::Delete => "delete",
            ChangeKind::Move => "move",
        }
    }
}

impl ChangeTracker {
    pub fn build_change(
        &self,
        tool_call_id: ToolCallId,
        before_path: Option<&camino::Utf8Path>,
        after_path: Option<&camino::Utf8Path>,
        before_text: Option<&str>,
        after_text: Option<&str>,
    ) -> Result<FileChange, crate::error::EditError> {
        let kind = match (before_text, after_text, before_path, after_path) {
            (None, Some(_), _, _) => ChangeKind::Add,
            (Some(_), None, _, _) => ChangeKind::Delete,
            (Some(_), Some(_), Some(before), Some(after)) if before != after => ChangeKind::Move,
            _ => ChangeKind::Update,
        };
        let diff_text = TextDiff::from_lines(before_text.unwrap_or(""), after_text.unwrap_or(""))
            .unified_diff()
            .header(
                before_path
                    .map(|value| value.as_str())
                    .unwrap_or("/dev/null"),
                after_path
                    .map(|value| value.as_str())
                    .unwrap_or("/dev/null"),
            )
            .to_string();

        Ok(FileChange {
            id: ChangeId::new(),
            tool_call_id,
            kind,
            path_before: before_path.map(|value| value.to_path_buf()),
            path_after: after_path.map(|value| value.to_path_buf()),
            before_sha256: before_text.map(sha256_hex),
            after_sha256: after_text.map(sha256_hex),
            summary: build_summary(kind, before_path, after_path),
            diff_text,
            created_at_ms: SystemClock::now_ms(),
        })
    }
}

pub(crate) fn path_for_change_storage(path: &Utf8Path, workspace_root: &Utf8Path) -> Utf8PathBuf {
    if let Some(relative) = crate::workspace::project::workspace_relative_key_for_match(
        path.as_str(),
        workspace_root.as_str(),
    ) {
        if !relative.is_empty() {
            return Utf8PathBuf::from(relative);
        }
    }
    path.strip_prefix(workspace_root)
        .map(|relative| relative.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn build_summary(
    kind: ChangeKind,
    before_path: Option<&camino::Utf8Path>,
    after_path: Option<&camino::Utf8Path>,
) -> String {
    match kind {
        ChangeKind::Add => format!("A {}", after_path.map(|value| value.as_str()).unwrap_or("")),
        ChangeKind::Update => {
            format!(
                "M {}",
                after_path
                    .or(before_path)
                    .map(|value| value.as_str())
                    .unwrap_or("")
            )
        }
        ChangeKind::Delete => {
            format!(
                "D {}",
                before_path.map(|value| value.as_str()).unwrap_or("")
            )
        }
        ChangeKind::Move => format!(
            "R {} -> {}",
            before_path.map(|value| value.as_str()).unwrap_or(""),
            after_path.map(|value| value.as_str()).unwrap_or("")
        ),
    }
}

fn render_display_path(path: Option<&Utf8PathBuf>, workspace_root: Option<&Utf8Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    if let Some(root) = workspace_root {
        if let Ok(relative) = path.strip_prefix(root) {
            return relative.as_str().to_string();
        }
    }
    path.as_str().to_string()
}
