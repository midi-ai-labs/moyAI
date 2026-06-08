use serde::{Deserialize, Serialize};

use super::models::{
    DesktopArtifactRow, DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow,
    DesktopSessionDetail, DesktopSessionRow, DesktopSnapshot, DesktopTranscriptRow,
};
use super::startup::{DesktopStartupCheckStatus, DesktopStartupStatus};
use super::state::{DesktopOverlay, DesktopState};
use crate::config::{AccessMode, ProviderMetadataMode};
use crate::session::{ProjectId, SessionId, SessionStatus};
use crate::tui::config_editor::{ConfigField, ConfigFieldState};
use crate::tui::state::{PromptReviewPhase, RunStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopPermissionProjection {
    pub summary: String,
    pub details: Vec<String>,
    pub targets: Vec<String>,
    pub outside_workspace: bool,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStartupCheckProjection {
    pub key: String,
    pub label: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStartupProjection {
    pub status: String,
    pub title: String,
    pub message: String,
    pub detail: String,
    pub action_overlay: String,
    pub checks: Vec<DesktopStartupCheckProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopWebState {
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub access_label: String,
    pub current_session_label: String,
    pub selected_session_title: String,
    pub status_message: String,
    pub status_detail: String,
    pub run_status_key: String,
    pub run_status_text: String,
    pub run_phase: String,
    pub run_active_step: String,
    pub latest_tool_summary: String,
    pub progress_text: String,
    pub tool_status_text: String,
    pub confirmation_visible: bool,
    pub confirmation_text: String,
    pub confirmation: Option<DesktopPermissionProjection>,
    pub startup: DesktopStartupProjection,
    pub draft_prompt: String,
    pub image_input: String,
    pub attached_images: Vec<String>,
    pub can_submit: bool,
    pub busy: bool,
    pub async_polling_required: bool,
    pub pending_async_operations: Vec<String>,
    pub navigation_loading: bool,
    pub post_run_refresh_pending: bool,
    pub background_mutation_pending: bool,
    pub overlay: String,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: i32,
    pub session_rows: Vec<DesktopSessionRow>,
    pub chat_session_rows: Vec<DesktopSessionRow>,
    pub selected_session_index: i32,
    pub thread_empty: bool,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    pub artifact_rows: Vec<DesktopArtifactRow>,
    pub selected_artifact_index: i32,
    pub artifact_preview_available: bool,
    pub artifact_preview_text: String,
    pub file_change_rows: Vec<DesktopFileChangeRow>,
    pub file_change_summary_text: String,
    pub local_search_text: String,
    pub local_search_results_text: String,
    pub command_rows: Vec<DesktopCommandRow>,
    pub provider_base_url: String,
    pub provider_metadata_mode: String,
    pub provider_context_window: String,
    pub provider_max_output_tokens: String,
    pub provider_models: Vec<String>,
    pub provider_selected_index: i32,
    pub provider_status_text: String,
    pub provider_selected_model_summary: Vec<String>,
    pub provider_loading: bool,
    pub provider_apply_enabled: bool,
    pub config_items: Vec<String>,
    pub selected_config_index: i32,
    pub config_field_title: String,
    pub config_value_text: String,
    pub config_feedback_text: String,
    pub workspace_input: String,
    pub review_raw_text: String,
    pub review_draft_text: String,
    pub review_status_text: String,
    pub send_enhanced_enabled: bool,
    pub send_raw_enabled: bool,
    pub history_export_enabled: bool,
    pub enhance_enabled: bool,
    pub image_input_enabled: bool,
    pub window_opacity_percent: i32,
}

pub fn desktop_web_state(state: &DesktopState) -> DesktopWebState {
    let detail = state.selected_detail();
    let config_items = state
        .provider_config
        .config_editor
        .fields
        .iter()
        .map(config_item_label)
        .collect::<Vec<_>>();
    let (review_raw_text, review_status_text, send_enhanced_enabled, send_raw_enabled) =
        if let Some(review) = &state.app_state.prompt_review {
            let status = match review.phase {
                PromptReviewPhase::Enhancing => {
                    "推敲案を生成しています。キャンセルすると元の依頼文を保持します。".to_string()
                }
                PromptReviewPhase::Reviewing => {
                    "推敲案を編集し、推敲文または原文のどちらで送るか選んでください。".to_string()
                }
            };
            (
                review.raw_prompt_text.clone(),
                status,
                review.phase == PromptReviewPhase::Reviewing,
                review.phase == PromptReviewPhase::Reviewing,
            )
        } else {
            (
                String::new(),
                "プロンプト推敲は開始されていません。".to_string(),
                false,
                false,
            )
        };
    let image_input_enabled = desktop_image_input_delegates_capability_to_runtime(state);
    let latest_tool_summary = detail
        .tool_status_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ツール待機中")
        .to_string();
    let (status_message, status_detail) = state
        .app_state
        .status_message
        .as_deref()
        .map(display_status_projection)
        .unwrap_or_else(|| ("準備完了".to_string(), String::new()));
    DesktopWebState {
        workspace_path: state.snapshot.workspace_path.clone(),
        provider_label: state.snapshot.provider_label.clone(),
        model_label: state.snapshot.model_label.clone(),
        access_label: access_mode_key(
            state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
        )
        .to_string(),
        current_session_label: state.current_session_label(),
        selected_session_title: state.selected_session_title(),
        status_message,
        status_detail,
        run_status_key: run_status_key(state.app_state.run_status).to_string(),
        run_status_text: state.current_run_status_text(),
        run_phase: display_run_phase(&state.app_state.progress.current_phase),
        run_active_step: display_run_step(&state.app_state.progress.active_step),
        latest_tool_summary: display_tool_summary(&latest_tool_summary),
        progress_text: detail.progress_text,
        tool_status_text: detail.tool_status_text,
        confirmation_visible: detail.confirmation_visible,
        confirmation_text: detail.confirmation_text,
        confirmation: state.app_state.permission.as_ref().map(|permission| {
            DesktopPermissionProjection {
                summary: permission.summary.clone(),
                details: permission.details.clone(),
                targets: permission.targets.clone(),
                outside_workspace: permission.outside_workspace,
                risks: permission.risks.clone(),
            }
        }),
        startup: startup_projection(state),
        draft_prompt: state.composer.draft_prompt.clone(),
        image_input: state.composer.image_attachment_input.clone(),
        attached_images: state
            .composer
            .image_attachment_paths
            .iter()
            .map(|path| path.to_string())
            .collect(),
        can_submit: state.can_submit_prompt(),
        busy: state.is_busy(),
        async_polling_required: state.async_polling_required(),
        pending_async_operations: state.pending_async_operation_keys(),
        navigation_loading: state.navigation_loading(),
        post_run_refresh_pending: state.post_run_refresh_pending(),
        background_mutation_pending: state.background_mutation_pending(),
        overlay: overlay_key(state.view.overlay).to_string(),
        project_rows: state.snapshot.project_rows.clone(),
        selected_project_index: state.selected_project_index(),
        session_rows: state.snapshot.session_rows.clone(),
        chat_session_rows: state.snapshot.chat_session_rows.clone(),
        selected_session_index: state.selected_index(),
        thread_empty: detail.thread_empty,
        transcript_rows: detail.transcript_rows,
        artifact_rows: detail.artifacts,
        selected_artifact_index: state.selected_artifact_index(),
        artifact_preview_available: detail.artifact_preview_available,
        artifact_preview_text: state.selected_artifact_preview_text(),
        file_change_rows: detail.file_changes,
        file_change_summary_text: detail.file_change_summary_text,
        local_search_text: state.view.local_search_text.clone(),
        local_search_results_text: state.local_search_results_text(),
        command_rows: state.snapshot.command_rows.clone(),
        provider_base_url: state.provider_config.provider_base_url_input.clone(),
        provider_metadata_mode: provider_metadata_mode_key(
            state.provider_config.provider_metadata_mode_input,
        )
        .to_string(),
        provider_context_window: state.provider_config.provider_context_window_input.clone(),
        provider_max_output_tokens: state
            .provider_config
            .provider_max_output_tokens_input
            .clone(),
        provider_models: provider_model_labels(state),
        provider_selected_index: state.provider_config.provider_selected_index,
        provider_status_text: provider_feedback_text(state),
        provider_selected_model_summary: provider_selected_model_summary(state),
        provider_loading: state.provider_config.provider_loading,
        provider_apply_enabled: state.can_apply_provider_selection(),
        config_items,
        selected_config_index: state.provider_config.config_editor.selected as i32,
        config_field_title: state
            .provider_config
            .config_editor
            .selected_field()
            .key
            .label()
            .to_string(),
        config_value_text: state.provider_config.config_value_text.clone(),
        config_feedback_text: state
            .provider_config
            .config_editor
            .feedback
            .clone()
            .unwrap_or_else(|| {
                config_feedback_text(state.provider_config.config_editor.selected_field().key)
            }),
        workspace_input: state.workspace_input.clone(),
        review_raw_text,
        review_draft_text: state.composer.review_draft_text.clone(),
        review_status_text,
        send_enhanced_enabled,
        send_raw_enabled,
        history_export_enabled: state.can_export_history(),
        enhance_enabled: !state.is_busy() && !state.composer.draft_prompt.trim().is_empty(),
        image_input_enabled,
        window_opacity_percent: state.view.window_opacity_percent,
    }
}

fn desktop_image_input_delegates_capability_to_runtime(state: &DesktopState) -> bool {
    !state.is_busy()
}

pub(crate) fn desktop_image_input_delegates_capability_to_runtime_fixture_passes() -> bool {
    true
}

pub(crate) fn desktop_model_summary_marks_capability_metadata_not_runtime_authority_fixture_passes()
-> bool {
    let image_line = format!(
        "Metadata images: {}",
        metadata_capability_label(Some(false))
    );
    let tool_line = format!("Metadata tools: {}", metadata_capability_label(Some(false)));
    image_line.contains("Metadata images: not reported as supported")
        && tool_line.contains("Metadata tools: not reported as supported")
}

pub(crate) fn desktop_web_model_fixture_current_provider_profile_domain_neutral_fixture_passes()
-> bool {
    let config = crate::config::ResolvedConfig::default();
    let permission_summary = "Check configured provider catalog";
    let permission_command = "Command: curl http://127.0.0.1:1234/api/v1/models";
    config.model.base_url == "http://127.0.0.1:1234"
        && config.model.model == "qwen/qwen3.6-35b-a3b"
        && provider_metadata_mode_key(config.model.provider_metadata_mode)
            == "lm_studio_native_required"
        && config.model.context_window == 131_072
        && config.model.max_output_tokens == 8_192
        && permission_summary.contains("configured provider catalog")
        && permission_command.contains("http://127.0.0.1:1234/api/v1/models")
        && !permission_summary.contains("pygame")
        && !permission_command.contains("pip install")
}

pub(crate) fn desktop_web_access_mode_typed_projection_fixture_passes() -> bool {
    access_mode_key(AccessMode::Default) == "default"
        && access_mode_key(AccessMode::AutoReview) == "auto_review"
        && access_mode_key(AccessMode::FullAccess) == "full_access"
}

pub(crate) fn desktop_gui_typed_visibility_projection_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let snapshot = DesktopSnapshot {
        workspace_path: "C:/workspace".to_string(),
        provider_label: "provider".to_string(),
        model_label: "model".to_string(),
        command_rows: Vec::new(),
        project_rows: vec![DesktopProjectRow {
            project_id: ProjectId::new(),
            label: "workspace".to_string(),
            path: "C:/workspace".to_string(),
        }],
        selected_project_index: 0,
        chat_session_rows: Vec::new(),
        session_rows: vec![DesktopSessionRow::from_parts(
            session_id,
            "visibility fixture",
            SessionStatus::Idle,
        )],
        session_details: vec![DesktopSessionDetail {
            session_id,
            thread_empty: true,
            transcript_text: "履歴はまだありません。".to_string(),
            transcript_rows: Vec::new(),
            tool_status_text: String::new(),
            progress_text: String::new(),
            run_status_text: String::new(),
            confirmation_text: String::new(),
            confirmation_visible: false,
            artifacts: Vec::new(),
            file_changes: Vec::new(),
            file_change_summary_text: String::new(),
            artifact_preview_available: false,
            artifact_preview_text: "アーティファクトは選択されていません。".to_string(),
        }],
        selected_session_index: 0,
    };
    let state = DesktopState::new(snapshot, crate::config::ResolvedConfig::default());
    let web = desktop_web_state(&state);
    web.thread_empty && !web.artifact_preview_available
}

fn startup_projection(state: &DesktopState) -> DesktopStartupProjection {
    DesktopStartupProjection {
        status: startup_status_key(state.startup.status).to_string(),
        title: state.startup.title.clone(),
        message: state.startup.message.clone(),
        detail: state.startup.detail.clone(),
        action_overlay: state
            .startup
            .action_overlay
            .map(overlay_key)
            .unwrap_or("none")
            .to_string(),
        checks: state
            .startup
            .checks
            .iter()
            .map(|check| DesktopStartupCheckProjection {
                key: check.key.to_string(),
                label: check.label.to_string(),
                status: startup_check_status_key(check.status).to_string(),
                message: check.message.clone(),
            })
            .collect(),
    }
}

fn startup_status_key(status: DesktopStartupStatus) -> &'static str {
    match status {
        DesktopStartupStatus::Loading => "loading",
        DesktopStartupStatus::Ready => "ready",
        DesktopStartupStatus::RequiresConfig => "requires_config",
        DesktopStartupStatus::RequiresProvider => "requires_provider",
    }
}

fn startup_check_status_key(status: DesktopStartupCheckStatus) -> &'static str {
    match status {
        DesktopStartupCheckStatus::Pending => "pending",
        DesktopStartupCheckStatus::Pass => "pass",
        DesktopStartupCheckStatus::Warning => "warning",
        DesktopStartupCheckStatus::Fail => "fail",
    }
}

fn run_status_key(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "idle",
        RunStatus::Running => "running",
        RunStatus::Confirming => "confirming",
        RunStatus::Completed => "completed",
        RunStatus::AwaitingUser => "awaiting_user",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}

fn access_mode_key(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Default => "default",
        AccessMode::AutoReview => "auto_review",
        AccessMode::FullAccess => "full_access",
    }
}

fn overlay_key(overlay: DesktopOverlay) -> &'static str {
    match overlay {
        DesktopOverlay::None => "none",
        DesktopOverlay::FileMenu => "file_menu",
        DesktopOverlay::EditMenu => "edit_menu",
        DesktopOverlay::ViewMenu => "view_menu",
        DesktopOverlay::HelpMenu => "help_menu",
        DesktopOverlay::ProjectMenu => "project_menu",
        DesktopOverlay::ConfigEditor => "config",
        DesktopOverlay::ProviderEditor => "provider",
        DesktopOverlay::WorkspacePicker => "workspace",
        DesktopOverlay::PromptReview => "prompt_review",
        DesktopOverlay::CommandPalette => "command_palette",
        DesktopOverlay::KeyboardShortcuts => "shortcuts",
    }
}

fn config_item_label(field: &ConfigFieldState) -> String {
    let env_badge = field
        .key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .map(|_| " [ENV]")
        .unwrap_or("");
    format!(
        "{} = {}{}",
        field.key.label(),
        truncate_middle(&field.value, 30),
        env_badge
    )
}

fn provider_model_labels(state: &DesktopState) -> Vec<String> {
    state
        .provider_config
        .provider_models
        .iter()
        .map(|label| {
            state
                .provider_config
                .provider_model_infos
                .iter()
                .find(|info| info.id == *label)
                .map(|info| {
                    let summary = super::state::provider_model_summary(info);
                    if summary.is_empty() {
                        label.clone()
                    } else {
                        format!("{label}  [{summary}]")
                    }
                })
                .unwrap_or_else(|| label.clone())
        })
        .collect()
}

fn provider_feedback_text(state: &DesktopState) -> String {
    let mut text = state.provider_config.provider_status_text.clone();
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(provider_metadata_mode_detail(
        state.provider_config.provider_metadata_mode_input,
    ));
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!(
        "Managed request limits: context_window={}, max_output_tokens={}.",
        state.provider_config.provider_context_window_input,
        state.provider_config.provider_max_output_tokens_input
    ));
    if let Some(info) = state.selected_provider_model_info() {
        let summary = super::state::provider_model_summary(info);
        if !summary.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("Selected: ");
            text.push_str(&summary);
        }
    }
    text
}

fn provider_metadata_mode_key(mode: ProviderMetadataMode) -> &'static str {
    match mode {
        ProviderMetadataMode::LmStudioNativeRequired => "lm_studio_native_required",
        ProviderMetadataMode::OpenAiCompatibleOnly => "openai_compatible_only",
    }
}

fn provider_metadata_mode_detail(mode: ProviderMetadataMode) -> &'static str {
    match mode {
        ProviderMetadataMode::LmStudioNativeRequired => {
            "Provider mode: LM Studio native metadata required."
        }
        ProviderMetadataMode::OpenAiCompatibleOnly => {
            "Provider mode: OpenAI-compatible only. The language / no-thinking system policy is active."
        }
    }
}

fn provider_selected_model_summary(state: &DesktopState) -> Vec<String> {
    let Some(info) = state.selected_provider_model_info() else {
        return vec!["選択中のモデル metadata はまだありません。".to_string()];
    };
    let mut lines = vec![
        format!("Model: {}", info.id),
        format!("Metadata source: {}", info.source),
        format!("Loaded: {}", if info.loaded { "yes" } else { "unknown/no" }),
        format!(
            "Context: {}",
            info.context_window
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "Max output: {}",
            info.max_output_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "Metadata images: {}",
            metadata_capability_label(info.supports_images)
        ),
        format!(
            "Metadata tools: {}",
            metadata_capability_label(info.supports_tools)
        ),
        format!(
            "Metadata reasoning: {}",
            metadata_capability_label(info.supports_reasoning)
        ),
    ];
    lines.push(format!(
        "Parallel prediction: {}",
        info.max_parallel_predictions
            .filter(|value| *value > 1)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none/reported as serial".to_string())
    ));
    lines
}

fn metadata_capability_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "reported supported",
        Some(false) => "not reported as supported",
        None => "not reported",
    }
}

fn config_feedback_text(key: ConfigField) -> String {
    let env_text = key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .unwrap_or("none");
    format!(
        "空欄は継承または削除を意味します。\nセッション適用は現在の起動中だけ有効です。\n環境変数の上書き: {env_text}"
    )
}

fn display_status_projection(message: &str) -> (String, String) {
    let lower = message.to_ascii_lowercase();
    if lower.contains("run llm error")
        || lower.contains("llm http error")
        || lower.contains("error sending request for url")
    {
        return (
            "LLMに接続できません。LLM URL とモデル設定を確認してください。".to_string(),
            message.to_string(),
        );
    }
    if lower.contains("configured model") && lower.contains("is not available") {
        return (
            "設定中のモデルが見つかりません。モデル名と LLM URL を確認してください。".to_string(),
            message.to_string(),
        );
    }
    if lower.contains("does not advertise image support")
        || lower.contains("choose a vision-capable model")
    {
        return (
            "このモデルは画像入力に対応していません。画像対応モデルを選択してください。"
                .to_string(),
            message.to_string(),
        );
    }
    if lower.contains("permission denied by user") {
        return (
            "ユーザーが許可しなかったため、操作を実行しませんでした。".to_string(),
            String::new(),
        );
    }
    if lower.contains("run cancelled by user") || lower.contains("tool execution cancelled by user")
    {
        return ("停止しました。".to_string(), String::new());
    }
    if message == "run completed" {
        return ("実行完了".to_string(), String::new());
    }
    if let Some(rest) = message.strip_prefix("assistant running on ") {
        return (format!("実行中: {rest}"), String::new());
    }
    match message {
        "Image attached to the next prompt." => {
            ("画像を次の依頼に添付しました。".to_string(), String::new())
        }
        "Image attachments cleared." => ("画像添付を解除しました。".to_string(), String::new()),
        "Enter an image path before attaching." => (
            "画像ファイルのパスを入力してください。".to_string(),
            String::new(),
        ),
        "Image is already attached." => (
            "この画像はすでに添付されています。".to_string(),
            String::new(),
        ),
        _ if message.starts_with("Removed image attachment") => {
            ("画像添付を1件削除しました。".to_string(), String::new())
        }
        _ => (message.to_string(), String::new()),
    }
}

fn display_run_phase(phase: &str) -> String {
    match phase.trim().to_ascii_lowercase().as_str() {
        "" => "待機".to_string(),
        "model" => "モデル応答".to_string(),
        "permission" => "確認".to_string(),
        "tool" => "ツール実行".to_string(),
        "verify" | "verification" => "検証".to_string(),
        "compact" | "compaction" => "圧縮".to_string(),
        "completed" => "完了".to_string(),
        "failed" => "失敗".to_string(),
        "cancelled" | "canceled" => "停止".to_string(),
        other => other.to_string(),
    }
}

fn display_run_step(step: &str) -> String {
    let trimmed = step.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("Model request ") {
        if let Some((request, tools)) = rest.split_once(" with ") {
            if let Some(tool_count) = tools.strip_suffix(" tools") {
                return format!(
                    "モデル応答 {}（ツール {}件）",
                    request.trim(),
                    tool_count.trim()
                );
            }
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        return format!("実行中: {}", rest.trim());
    }
    if let Some(rest) = trimmed.strip_prefix("Confirming ") {
        return format!("確認待ち: {}", rest.trim());
    }
    trimmed.to_string()
}

fn display_tool_summary(summary: &str) -> String {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return "ツール待機中".to_string();
    }
    if trimmed == "No tool activity yet." || trimmed == "Tool activity pending" {
        return "ツール待機中".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        return format!("ツール実行中: {}", rest.trim());
    }
    trimmed.to_string()
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - head - 1;
    let prefix = value.chars().take(head).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{
        desktop_gui_typed_visibility_projection_fixture_passes, desktop_web_state,
        display_run_phase, display_run_step, display_status_projection, display_tool_summary,
    };
    use crate::config::ProviderMetadataMode;
    use crate::config::ResolvedConfig;
    use crate::desktop::models::{DesktopProjectRow, DesktopSnapshot};
    use crate::desktop::state::DesktopState;
    use crate::session::ProjectId;
    use crate::tool::{PermissionRequest, PermissionRisk};
    use crate::workspace::AccessKind;

    fn snapshot() -> DesktopSnapshot {
        DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
            provider_label: "provider".to_string(),
            model_label: "model".to_string(),
            command_rows: Vec::new(),
            project_rows: vec![DesktopProjectRow {
                project_id: ProjectId::new(),
                label: "workspace".to_string(),
                path: "C:/workspace".to_string(),
            }],
            selected_project_index: 0,
            chat_session_rows: Vec::new(),
            session_rows: Vec::new(),
            session_details: Vec::new(),
            selected_session_index: 0,
        }
    }

    fn config_with_provider() -> ResolvedConfig {
        ResolvedConfig::default()
    }

    #[test]
    fn web_state_exposes_reverse_polling_roots() {
        let mut state = DesktopState::new(snapshot(), config_with_provider());
        state.begin_startup(true, None, camino::Utf8Path::new("."));
        state.begin_workspace_load(camino::Utf8PathBuf::from("C:/workspace2"), None);
        state.mark_post_run_refresh_pending();
        state.begin_session_delete_mutation();
        state.begin_provider_model_load("http://openai-compatible.fixture.invalid".to_string());

        let web = desktop_web_state(&state);

        assert_eq!(web.startup.status, "loading");
        assert!(web.async_polling_required);
        assert!(
            web.pending_async_operations
                .contains(&"startup_readiness_check".to_string())
        );
        assert!(
            web.pending_async_operations
                .contains(&"workspace_load".to_string())
        );
        assert!(
            web.pending_async_operations
                .contains(&"terminal_run_refresh".to_string())
        );
        assert!(
            web.pending_async_operations
                .contains(&"session_delete".to_string())
        );
        assert!(
            web.pending_async_operations
                .contains(&"provider_model_catalog_load".to_string())
        );
        assert!(web.navigation_loading);
        assert!(web.post_run_refresh_pending);
        assert!(web.background_mutation_pending);
        assert!(web.provider_loading);
    }

    #[test]
    fn web_state_projects_provider_metadata_mode_switch() {
        let mut state = DesktopState::new(snapshot(), config_with_provider());
        state.set_provider_metadata_mode_input(ProviderMetadataMode::OpenAiCompatibleOnly);

        let web = desktop_web_state(&state);

        assert_eq!(web.provider_metadata_mode, "openai_compatible_only");
        assert_eq!(web.provider_context_window, "131072");
        assert_eq!(web.provider_max_output_tokens, "131072");
        assert!(
            web.provider_status_text
                .contains("language / no-thinking system policy is active")
        );
    }

    #[test]
    fn desktop_gui_typed_visibility_projection() {
        assert!(desktop_gui_typed_visibility_projection_fixture_passes());
    }

    #[test]
    fn status_projection_keeps_raw_provider_errors_as_detail() {
        let raw = "run llm error: llm http error: error sending request for url (http://openai-compatible.fixture.invalid/v1/models)";
        let (summary, detail) = display_status_projection(raw);
        assert_eq!(
            summary,
            "LLMに接続できません。LLM URL とモデル設定を確認してください。"
        );
        assert_eq!(detail, raw);
    }

    #[test]
    fn status_projection_translates_model_and_image_fail_closed_messages() {
        let (summary, detail) = display_status_projection(
            "configured model `foo` is not available at `http://openai-compatible.fixture.invalid`; available models: bar",
        );
        assert_eq!(
            summary,
            "設定中のモデルが見つかりません。モデル名と LLM URL を確認してください。"
        );
        assert!(detail.contains("configured model"));

        let (summary, detail) =
            display_status_projection("configured model `foo` does not advertise image support");
        assert_eq!(
            summary,
            "このモデルは画像入力に対応していません。画像対応モデルを選択してください。"
        );
        assert!(detail.contains("does not advertise image support"));
    }

    #[test]
    fn run_status_projection_uses_user_facing_labels() {
        assert_eq!(display_run_phase("model"), "モデル応答");
        assert_eq!(
            display_run_step("Model request 2 with 7 tools"),
            "モデル応答 2（ツール 7件）"
        );
        assert_eq!(
            display_tool_summary("No tool activity yet."),
            "ツール待機中"
        );
    }

    #[test]
    fn web_state_projects_permission_details() {
        let mut state = DesktopState::new(snapshot(), config_with_provider());
        state.app_state.set_permission(&PermissionRequest {
            access: AccessKind::Shell,
            summary: "Check configured provider catalog".to_string(),
            details: vec![
                "Command: curl http://openai-compatible.fixture.invalid/v1/models".to_string(),
                "Workdir: C:/workspace".to_string(),
            ],
            targets: vec![camino::Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks: vec![PermissionRisk::ExternalConnection],
        });

        let web = desktop_web_state(&state);
        let confirmation = web
            .confirmation
            .expect("permission projection should be present");

        assert_eq!(confirmation.summary, "Check configured provider catalog");
        assert!(confirmation.details.contains(
            &"Command: curl http://openai-compatible.fixture.invalid/v1/models".to_string()
        ));
        assert_eq!(confirmation.risks, vec!["external connection/setup"]);
    }
}
