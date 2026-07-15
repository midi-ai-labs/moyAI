use camino::{Utf8Path, Utf8PathBuf};

use crate::config::ResolvedConfig;
use crate::docling::normalize_docling_base_url;
use crate::llm::normalize_provider_base_url;

use super::state::DesktopOverlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopStartupStatus {
    Ready,
    RequiresConfig,
    RequiresProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopStartupCheckStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Debug, Clone)]
pub struct DesktopStartupCheck {
    pub key: &'static str,
    pub label: &'static str,
    pub status: DesktopStartupCheckStatus,
    pub message: String,
}

impl DesktopStartupCheck {
    fn pass(key: &'static str, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            key,
            label,
            status: DesktopStartupCheckStatus::Pass,
            message: message.into(),
        }
    }

    fn warning(key: &'static str, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            key,
            label,
            status: DesktopStartupCheckStatus::Warning,
            message: message.into(),
        }
    }

    fn fail(key: &'static str, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            key,
            label,
            status: DesktopStartupCheckStatus::Fail,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DesktopStartupState {
    pub status: DesktopStartupStatus,
    pub title: String,
    pub message: String,
    pub detail: String,
    pub action_overlay: Option<DesktopOverlay>,
    pub checks: Vec<DesktopStartupCheck>,
    config_requires_attention: bool,
}

impl Default for DesktopStartupState {
    fn default() -> Self {
        Self::ready()
    }
}

impl DesktopStartupState {
    pub fn ready() -> Self {
        Self {
            status: DesktopStartupStatus::Ready,
            title: "moyAI".to_string(),
            message: "起動準備が完了しました。".to_string(),
            detail: String::new(),
            action_overlay: None,
            checks: Vec::new(),
            config_requires_attention: false,
        }
    }

    pub fn begin(
        global_config_existed_at_launch: bool,
        global_config_path: Option<Utf8PathBuf>,
        workspace_root: &Utf8Path,
        config: &ResolvedConfig,
    ) -> Self {
        let mut checks = Vec::new();
        let config_message = match (global_config_existed_at_launch, global_config_path) {
            (true, Some(path)) => {
                format!("設定ファイルを確認しました: {path}")
            }
            (true, None) => "設定ファイルのパスを解決済みです。".to_string(),
            (false, Some(path)) => {
                format!("初回起動用の既定設定を作成しました: {path}")
            }
            (false, None) => "初回起動用の既定設定を作成しました。".to_string(),
        };
        checks.push(if global_config_existed_at_launch {
            DesktopStartupCheck::pass("config", "設定ファイル", config_message)
        } else {
            DesktopStartupCheck::warning("config", "設定ファイル", config_message)
        });

        checks.push(if workspace_root.as_std_path().is_dir() {
            DesktopStartupCheck::pass("workspace", "ワークスペース", format!("{workspace_root}"))
        } else {
            DesktopStartupCheck::warning(
                "workspace",
                "ワークスペース",
                format!("ワークスペースを確認してください: {workspace_root}"),
            )
        });

        checks.push(Self::provider_config_check(config));
        checks.push(Self::docling_config_check(config));

        let mut state = Self {
            status: DesktopStartupStatus::Ready,
            title: "moyAI".to_string(),
            message: "ローカル設定を確認しました。".to_string(),
            detail: "起動時に provider や Docling への network request は送信しません。"
                .to_string(),
            action_overlay: None,
            checks,
            config_requires_attention: !global_config_existed_at_launch,
        };
        state.recompute();
        state
    }

    pub fn refresh_config(&mut self, config: &ResolvedConfig) {
        self.set_check(Self::provider_config_check(config));
        self.set_check(Self::docling_config_check(config));
        self.recompute();
    }

    pub fn mark_config_reviewed(&mut self) {
        self.config_requires_attention = false;
        self.recompute();
    }

    pub fn requires_initial_setup(&self) -> bool {
        self.config_requires_attention
    }

    fn set_check(&mut self, check: DesktopStartupCheck) {
        if let Some(existing) = self.checks.iter_mut().find(|item| item.key == check.key) {
            *existing = check;
        } else {
            self.checks.push(check);
        }
    }

    fn provider_config_check(config: &ResolvedConfig) -> DesktopStartupCheck {
        let base_url = normalize_provider_base_url(&config.model.base_url);
        let model = config.model.model.trim();
        if base_url.is_empty() {
            return DesktopStartupCheck::fail("provider", "LLM 設定", "LLM URL が未設定です。");
        }
        if model.is_empty() {
            return DesktopStartupCheck::fail("provider", "LLM 設定", "model が未設定です。");
        }
        DesktopStartupCheck::pass(
            "provider",
            "LLM 設定",
            format!(
                "設定済み: {base_url} / {model}。接続は依頼実行時または明示的なモデル読込で確認します。"
            ),
        )
    }

    fn docling_config_check(config: &ResolvedConfig) -> DesktopStartupCheck {
        if !config.docling.enabled {
            return DesktopStartupCheck::pass(
                "docling",
                "Docling 設定",
                "無効です。structured document 処理が必要な場合は設定から有効化してください。",
            );
        }
        let base_url = normalize_docling_base_url(&config.docling.base_url);
        if base_url.is_empty() {
            return DesktopStartupCheck::fail(
                "docling",
                "Docling 設定",
                "Docling Serve URL が未設定です。",
            );
        }
        DesktopStartupCheck::pass(
            "docling",
            "Docling 設定",
            format!("設定済み: {base_url}。接続はDocling利用時に確認します。"),
        )
    }

    fn recompute(&mut self) {
        let provider_failed = self.checks.iter().any(|check| {
            check.key == "provider" && check.status == DesktopStartupCheckStatus::Fail
        });
        let docling_failed = self
            .checks
            .iter()
            .any(|check| check.key == "docling" && check.status == DesktopStartupCheckStatus::Fail);
        if self.config_requires_attention {
            self.status = DesktopStartupStatus::RequiresConfig;
            self.title = "設定の確認が必要です".to_string();
            self.message =
                "初回起動用の設定を作成しました。起動後に設定画面を開きます。".to_string();
            self.detail = "LLM URL、model、権限 preset を確認してください。".to_string();
            self.action_overlay = Some(DesktopOverlay::ConfigEditor);
            return;
        }

        if provider_failed {
            self.status = DesktopStartupStatus::RequiresProvider;
            self.title = "LLM 設定の確認が必要です".to_string();
            self.message = "起動後に LLM URL 画面を開きます。".to_string();
            self.detail = "base URL と model を設定してください。".to_string();
            self.action_overlay = Some(DesktopOverlay::ProviderEditor);
            return;
        }

        if docling_failed {
            self.status = DesktopStartupStatus::RequiresConfig;
            self.title = "Docling 設定の確認が必要です".to_string();
            self.message = "起動後に設定画面を開きます。".to_string();
            self.detail = "docling.enabled と docling.base_url を確認してください。".to_string();
            self.action_overlay = Some(DesktopOverlay::ConfigEditor);
            return;
        }

        self.status = DesktopStartupStatus::Ready;
        self.title = "moyAI".to_string();
        self.message = "ローカル設定を確認しました。".to_string();
        self.detail =
            "provider catalogとavailability diagnosticsは明示操作時だけnetworkへ接続します。"
                .to_string();
        self.action_overlay = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_startup_completes_from_local_values_only() {
        let config = ResolvedConfig::default();
        let state = DesktopStartupState::begin(true, None, Utf8Path::new("."), &config);

        assert_eq!(state.status, DesktopStartupStatus::Ready);
        assert_eq!(state.action_overlay, None);
        assert!(state.checks.iter().all(|check| {
            matches!(
                check.status,
                DesktopStartupCheckStatus::Pass | DesktopStartupCheckStatus::Warning
            )
        }));
    }

    #[test]
    fn config_refresh_replaces_provider_and_docling_checks_without_pending_state() {
        let mut invalid = ResolvedConfig::default();
        invalid.model.base_url.clear();
        invalid.docling.enabled = true;
        invalid.docling.base_url.clear();
        let mut state = DesktopStartupState::begin(true, None, Utf8Path::new("."), &invalid);

        assert_eq!(state.status, DesktopStartupStatus::RequiresProvider);
        assert_eq!(
            state
                .checks
                .iter()
                .filter(|check| check.key == "provider" || check.key == "docling")
                .count(),
            2
        );
        assert!(state.checks.iter().any(|check| {
            check.key == "provider" && check.status == DesktopStartupCheckStatus::Fail
        }));

        let mut valid = ResolvedConfig::default();
        valid.docling.enabled = true;
        valid.docling.base_url = "http://127.0.0.1:8123".to_string();
        state.refresh_config(&valid);

        assert_eq!(state.status, DesktopStartupStatus::Ready);
        assert!(state.checks.iter().all(|check| {
            check.key != "provider" && check.key != "docling"
                || check.status == DesktopStartupCheckStatus::Pass
        }));
    }
}
