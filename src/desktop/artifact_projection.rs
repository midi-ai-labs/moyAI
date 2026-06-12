use camino::Utf8Path;

use crate::desktop::models::{DesktopArtifactRow, DesktopFileChangeRow};
use crate::protocol::{
    FileChangeEvidence, TurnItem, TurnItemPayload, turn_items_in_projection_order,
};
use crate::session::{ChangeKind, ToolCallId};
pub fn file_change_rows_from_turn_items(turn_items: &[TurnItem]) -> Vec<DesktopFileChangeRow> {
    file_change_rows_from_turn_items_with_root(turn_items, None)
}

pub(crate) fn file_change_rows_from_turn_items_with_root(
    turn_items: &[TurnItem],
    workspace_root: Option<&Utf8Path>,
) -> Vec<DesktopFileChangeRow> {
    let mut rows = Vec::new();
    for item in turn_items_in_projection_order(turn_items) {
        if let TurnItemPayload::FileChange {
            call_id,
            changes,
            summary,
            ..
        } = &item.payload
        {
            rows.extend(
                changes.iter().map(|change| {
                    file_change_row(*call_id, change, summary.as_str(), workspace_root)
                }),
            );
        }
    }
    dedupe_file_change_rows(rows)
}

fn file_change_row(
    call_id: ToolCallId,
    change: &FileChangeEvidence,
    fallback_summary: &str,
    workspace_root: Option<&Utf8Path>,
) -> DesktopFileChangeRow {
    let raw_path = change
        .path_after
        .as_ref()
        .or(change.path_before.as_ref())
        .map(|value| value.to_string());
    let path = raw_path
        .as_deref()
        .map(|value| display_file_change_path(Utf8Path::new(value), workspace_root))
        .unwrap_or_else(|| "(不明なパス)".to_string());
    let label = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(path.as_str())
        .to_string();
    let raw_summary = if change.summary.trim().is_empty() {
        fallback_summary.trim().to_string()
    } else {
        change.summary.trim().to_string()
    };
    let summary = normalize_file_change_summary(&raw_summary, raw_path.as_deref(), &path);
    DesktopFileChangeRow {
        label,
        path,
        kind: change.kind,
        action: change_kind_label(change.kind).to_string(),
        summary,
        tool_call_ids: vec![call_id],
    }
}

fn display_file_change_path(path: &Utf8Path, workspace_root: Option<&Utf8Path>) -> String {
    let display_path = workspace_root
        .and_then(|root| path.strip_prefix(root).ok())
        .filter(|relative| !relative.as_str().trim().is_empty())
        .unwrap_or(path);
    display_path.as_str().replace('\\', "/")
}

fn normalize_file_change_summary(
    summary: &str,
    raw_path: Option<&str>,
    display_path: &str,
) -> String {
    let mut normalized = summary.to_string();
    if let Some(raw_path) = raw_path {
        for candidate in file_path_summary_variants(raw_path) {
            normalized = normalized.replace(&candidate, display_path);
        }
    }
    normalized
}

fn file_path_summary_variants(path: &str) -> Vec<String> {
    let mut variants = Vec::new();
    for candidate in [
        path.to_string(),
        path.replace('\\', "/"),
        path.replace('/', "\\"),
    ] {
        if !candidate.is_empty() && !variants.contains(&candidate) {
            variants.push(candidate);
        }
    }
    variants
}

pub(crate) fn artifact_rows_from_file_changes(
    rows: &[DesktopFileChangeRow],
) -> Vec<DesktopArtifactRow> {
    let mut artifacts = rows
        .iter()
        .map(|row| DesktopArtifactRow {
            label: row.label.clone(),
            path: row.path.clone(),
            kind: "ファイル".to_string(),
            action: change_kind_label(row.kind).to_string(),
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    artifacts.dedup_by(|left, right| left.path == right.path);
    artifacts
}

fn dedupe_file_change_rows(rows: Vec<DesktopFileChangeRow>) -> Vec<DesktopFileChangeRow> {
    let mut deduped: Vec<DesktopFileChangeRow> = Vec::new();
    for row in rows {
        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.path == row.path)
        {
            existing.kind = merged_file_change_kind(existing.kind, row.kind);
            existing.action = change_kind_label(existing.kind).to_string();
            if !row.summary.trim().is_empty() {
                existing.summary = row.summary;
            }
            for call_id in row.tool_call_ids {
                if !existing.tool_call_ids.contains(&call_id) {
                    existing.tool_call_ids.push(call_id);
                }
            }
        } else {
            deduped.push(row);
        }
    }
    deduped
}

fn merged_file_change_kind(existing: ChangeKind, incoming: ChangeKind) -> ChangeKind {
    if matches!(existing, ChangeKind::Add) || matches!(incoming, ChangeKind::Add) {
        ChangeKind::Add
    } else if matches!(incoming, ChangeKind::Delete) {
        ChangeKind::Delete
    } else if matches!(incoming, ChangeKind::Move) {
        ChangeKind::Move
    } else {
        ChangeKind::Update
    }
}

pub(crate) fn format_file_change_summary(rows: &[DesktopFileChangeRow]) -> String {
    if rows.is_empty() {
        return "ファイル変更はまだありません。".to_string();
    }
    let added = rows
        .iter()
        .filter(|row| matches!(row.kind, ChangeKind::Add))
        .count();
    let updated = rows
        .iter()
        .filter(|row| matches!(row.kind, ChangeKind::Update))
        .count();
    let deleted = rows
        .iter()
        .filter(|row| matches!(row.kind, ChangeKind::Delete))
        .count();
    let moved = rows
        .iter()
        .filter(|row| matches!(row.kind, ChangeKind::Move))
        .count();
    let mut lines = vec![format!(
        "{}件のファイル変更（追加{} / 更新{} / 削除{} / 移動{}）",
        rows.len(),
        added,
        updated,
        deleted,
        moved
    )];
    lines.extend(rows.iter().take(8).map(|row| {
        let action = change_kind_label(row.kind);
        if row.summary.trim().is_empty() {
            format!("- [{}] {}", action, row.path)
        } else {
            format!("- [{}] {} - {}", action, row.path, row.summary)
        }
    }));
    lines.join("\n")
}

pub fn format_artifact_preview(
    artifact: Option<&DesktopArtifactRow>,
    changes: &[DesktopFileChangeRow],
) -> String {
    let Some(artifact) = artifact else {
        return "アーティファクトは選択されていません。".to_string();
    };
    let mut lines = vec![
        format!("アーティファクト: {}", artifact.label),
        format!("パス: {}", artifact.path),
        format!("種別: {}", artifact.kind),
        format!("操作: {}", artifact.action),
    ];
    if let Some(change) = changes.iter().find(|change| change.path == artifact.path) {
        if !change.summary.trim().is_empty() {
            lines.push(String::new());
            lines.push(change.summary.clone());
        }
    }
    lines.push(String::new());
    lines.push(
        "差分はセッション履歴のファイル変更から確認できます。Undo は安全契約を増やすため、この画面には露出していません。"
            .to_string(),
    );
    lines.join("\n")
}

fn change_kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Add => "追加",
        ChangeKind::Update => "更新",
        ChangeKind::Delete => "削除",
        ChangeKind::Move => "移動",
    }
}

#[cfg(test)]
mod tests {}
