//! Bounded Hub projection. Read/render the latest records before older large bodies.
use super::*;

const BLOCK_BYTES: usize = 8 * 1024;

pub(super) fn render(
    row: &McpHistoryRow,
    evidence: &Evidence,
    result: Option<&str>,
) -> (String, bool) {
    let mut output = String::from("# MCP履歴 — Hub向け最新記録\n\n");
    let mut truncated = evidence.truncated;
    output.push_str("各区分は新しい記録から表示します。取得時点の保存済み記録であり、現在の稼働や未記録の待機理由を保証しません。\n\n");
    let cutoff = evidence.history.last().map_or_else(
        || "指示・実行ログ: 記録なし".into(),
        |item| {
            format!(
                "指示・実行ログ: 記録 {} / Unix ms {}",
                item.sequence,
                item.at_ms
                    .map_or_else(|| "不明".into(), |time| time.to_string())
            )
        },
    );
    let progress = evidence.progress.last().map_or_else(
        || "進行記録: 記録なし".into(),
        |item| format!("進行記録: {}", item.sequence),
    );
    let wait = evidence.waits.last().map_or_else(
        || "待機記録: 記録なし".into(),
        |item| {
            format!(
                "待機記録: ターン {} / 記録 {} / Unix ms {}",
                item.turn_id, item.sequence, item.at_ms
            )
        },
    );
    append_block(
        &mut output,
        "取得対象の末尾（読み取り打切り位置）",
        &format!("{cutoff}\n{progress}\n{wait}"),
        2048,
        &mut truncated,
    );
    let metadata = format!(
        "履歴ID: {}\n方向: {:?}\n状態: {}\n状態の情報源: {}\n停止確認: {}\n結果受領: {}\nローカルセッションID: {}\nジョブID: {}\nルートタスクID: {}\n相手端末: {}\n対象: {}\n指示概要: {}",
        row.id,
        row.direction,
        row.state,
        row.state_source,
        row.stop_status,
        row.result_received,
        row.session_id,
        row.job_id.as_deref().unwrap_or("未受付・不明"),
        row.root_task_id,
        row.peer_label,
        row.target_label,
        row.title,
    );
    append_block(&mut output, "概要", &metadata, 2048, &mut truncated);
    let history = evidence.history.iter().rev().map(|item| {
        let text = item
            .payload
            .as_deref()
            .and_then(|text| serde_json::from_str::<HistoryItemPayload>(text).ok())
            .map(public_history_text)
            .unwrap_or_else(|| {
                "この記録の本文は読み取り上限または形式不一致により省略しました。".into()
            });
        (
            format!(
                "記録 {} / Unix ms {}",
                item.sequence,
                item.at_ms
                    .map_or_else(|| "不明".into(), |time| time.to_string())
            ),
            text,
        )
    });
    append_section(
        &mut output,
        "指示・実行ログ（新しい順）",
        history,
        34 * 1024,
        &mut truncated,
    );
    let waits = evidence.waits.iter().rev().map(|item| {
        (
            format!(
                "待機記録 {} / Unix ms {} / ターン {}",
                item.sequence, item.at_ms, item.turn_id
            ),
            item.text.clone(),
        )
    });
    append_section(
        &mut output,
        "この委任の待機・結果取得（新しい順）",
        waits,
        12 * 1024,
        &mut truncated,
    );
    let progress = evidence.progress.iter().rev().map(|item| {
        let text = item
            .payload
            .as_deref()
            .and_then(|text| serde_json::from_str::<TurnItemPayload>(text).ok())
            .map(public_progress_text)
            .unwrap_or_else(|| {
                "この記録の本文は読み取り上限または形式不一致により省略しました。".into()
            });
        (format!("進行記録 {}", item.sequence), text)
    });
    append_section(
        &mut output,
        "進行・承認・停止（新しい順）",
        progress,
        8 * 1024,
        &mut truncated,
    );
    if let Some(result) = result {
        append_block(
            &mut output,
            "保存済みの返却結果",
            result,
            2 * 1024,
            &mut truncated,
        );
    }
    if truncated {
        output.push_str("\n> 省略あり: 古い記録・大きい本文・読み取り上限を超えた本文は省略しています。本文は64 KiBまで、各区分の先頭・末尾各128件以内、1記録256 KiB、合計読み取り2 MiB以内です。表示した記録番号の間も連続とは限りません。詳細の再取得は最新の打切り位置で読み直します。より詳しい記録の確認・保存は元のDesktopのMCP履歴で行ってください。Desktop側にも表示上限があります。\n");
    }
    debug_assert!(output.len() <= HUB_MARKDOWN_BYTES);
    (output, truncated)
}

fn append_section(
    output: &mut String,
    title: &str,
    items: impl Iterator<Item = (String, String)>,
    budget: usize,
    truncated: &mut bool,
) {
    let start = output.len();
    output.push_str(&format!("## {title}\n\n"));
    let mut displayed = false;
    for (heading, text) in items {
        let available = budget.saturating_sub(output.len() - start);
        if available < 512 {
            *truncated = true;
            break;
        }
        append_block(
            output,
            &heading,
            &text,
            available.min(BLOCK_BYTES),
            truncated,
        );
        displayed = true;
    }
    if !displayed {
        output.push_str("対応する記録はありません。\n\n");
    }
}

/// Keep both ends of an oversized body: the end often contains the actual error.
/// Indent untrusted contents before clipping so neither edge creates Markdown instructions.
fn append_block(output: &mut String, title: &str, text: &str, budget: usize, truncated: &mut bool) {
    let heading = format!("### {title}\n\n");
    let body: String = text.lines().map(|line| format!("    {line}\n")).collect();
    if heading.len() + body.len() + 1 <= budget {
        output.push_str(&heading);
        output.push_str(&body);
        output.push('\n');
        return;
    }
    *truncated = true;
    const NOTICE: &str = "\n    […本文の途中を省略…]\n    ";
    let available = budget.saturating_sub(heading.len() + NOTICE.len() + 2);
    let mut head = available / 2;
    while !body.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = body.len().saturating_sub(available - head);
    while !body.is_char_boundary(tail) {
        tail += 1;
    }
    output.push_str(&heading);
    output.push_str(&body[..head]);
    output.push_str(NOTICE);
    output.push_str(&body[tail..]);
    output.push_str("\n\n");
}
