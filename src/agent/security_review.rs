use crate::session::{DurableFeedbackCategory, DurableFeedbackSeverity, DurableRuntimeFeedback};
use crate::storage::session_repo::SecurityReviewState;

/// Report the evidence boundary without promoting a model's review to a safety
/// verdict or retrying an unfinished review through another model loop.
pub(super) fn completion_feedback(state: &SecurityReviewState) -> Option<DurableRuntimeFeedback> {
    if state.latest_effect_position.is_none()
        && state.unclassified_tool_count == 0
        && state.latest_review_position.is_none()
    {
        return None;
    }
    let (severity, message) = if state.unclassified_tool_count > 0 {
        (
            DurableFeedbackSeverity::Warning,
            "過去のTool操作に分類不明の記録があるため、セキュリティレビューの対象を確定できません。安全性は未確認です。".to_owned(),
        )
    } else if !state.is_current() {
        let message = if state.latest_review_position.is_some() {
            "セキュリティレビュー後に変更を起こし得る操作があり、再レビューが記録されていません。現在の結果の安全性は未確認です。"
        } else {
            "変更を起こし得る操作がありましたが、セキュリティレビューが記録されていません。安全性は未確認です。"
        };
        (DurableFeedbackSeverity::Warning, message.to_owned())
    } else if state.successful_check_count == 0 {
        (
            DurableFeedbackSeverity::Warning,
            "セキュリティレビューは記録されましたが、現在の変更境界に対応する成功した確認操作を照合できませんでした。記録の未確認範囲を確認してください。".to_owned(),
        )
    } else if state.finding_count > 0 {
        (
            DurableFeedbackSeverity::Warning,
            format!(
                "セキュリティレビューに未解決の指摘{}件が記録されています。作業ターンの完了は、これらの解決や安全性の保証を意味しません。",
                state.finding_count
            ),
        )
    } else if state.check_count > state.successful_check_count {
        (
            DurableFeedbackSeverity::Warning,
            "セキュリティレビューには、失敗または現在の変更境界に対応しない確認操作が含まれています。これらを成功した確認として扱わず、レビュー記録を確認してください。".to_owned(),
        )
    } else if state.limitation_count > 0 {
        (
            DurableFeedbackSeverity::Info,
            format!(
                "セキュリティレビューには確認範囲の限界が{}件あります。確認内容と未確認範囲はレビュー記録を参照してください。",
                state.limitation_count
            ),
        )
    } else {
        return None;
    };
    Some(DurableRuntimeFeedback::new(
        severity,
        DurableFeedbackCategory::Runtime,
        message,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SecurityReviewState {
        SecurityReviewState {
            latest_effect_position: None,
            latest_effect_item_id: None,
            latest_review_position: None,
            reviewed_effect_position: None,
            reviewed_effect_item_id: None,
            check_count: 0,
            successful_check_count: 0,
            finding_count: 0,
            limitation_count: 0,
            unclassified_tool_count: 0,
        }
    }

    #[test]
    fn completion_distinguishes_read_only_missing_stale_and_current_evidence() {
        let mut state = state();
        assert!(completion_feedback(&state).is_none());
        state.latest_effect_position = Some(8);
        state.latest_effect_item_id = Some(crate::protocol::HistoryItemId::new());
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
        state.latest_review_position = Some(10);
        state.reviewed_effect_position = Some(8);
        state.reviewed_effect_item_id = state.latest_effect_item_id;
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
        state.check_count = 1;
        state.successful_check_count = 1;
        assert!(completion_feedback(&state).is_none());
        state.latest_effect_position = Some(12);
        state.latest_effect_item_id = Some(crate::protocol::HistoryItemId::new());
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
    }

    #[test]
    fn completion_reports_unknown_failed_and_limited_reviews_without_claiming_a_pass() {
        let mut state = state();
        state.latest_review_position = Some(4);
        state.check_count = 1;
        state.successful_check_count = 1;
        assert!(completion_feedback(&state).is_none());
        state.limitation_count = 1;
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Info
        );
        state.check_count = 2;
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
        state.finding_count = 1;
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
        state.unclassified_tool_count = 1;
        assert_eq!(
            completion_feedback(&state).unwrap().severity,
            DurableFeedbackSeverity::Warning
        );
    }
}
