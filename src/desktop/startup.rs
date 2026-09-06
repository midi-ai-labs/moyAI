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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopInitialSetupReason {
    ConfigMissing,
    ProviderInvalid,
    OptionalToolInvalid,
}

impl DesktopInitialSetupReason {
    pub const fn key(self) -> &'static str {
        match self {
            Self::ConfigMissing => "config_missing",
            Self::ProviderInvalid => "provider_invalid",
            Self::OptionalToolInvalid => "optional_tool_invalid",
        }
    }
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
    pub global_config_path: Option<Utf8PathBuf>,
    pub setup_generation: u64,
    pub initial_setup_reason: Option<DesktopInitialSetupReason>,
    setup_completion_pending: bool,
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
            global_config_path: None,
            setup_generation: 0,
            initial_setup_reason: None,
            setup_completion_pending: false,
        }
    }

    pub fn begin(
        global_config_existed_at_launch: bool,
        global_config_path: Option<Utf8PathBuf>,
        workspace_root: &Utf8Path,
        config: &ResolvedConfig,
    ) -> Self {
        let mut checks = Vec::new();
        let config_message = match (global_config_existed_at_launch, global_config_path.as_ref()) {
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

        let initial_setup_reason =
            if !global_config_existed_at_launch {
                Some(DesktopInitialSetupReason::ConfigMissing)
            } else if checks.iter().any(|check| {
                check.key == "provider" && check.status == DesktopStartupCheckStatus::Fail
            }) {
                Some(DesktopInitialSetupReason::ProviderInvalid)
            } else if checks.iter().any(|check| {
                check.key == "docling" && check.status == DesktopStartupCheckStatus::Fail
            }) {
                Some(DesktopInitialSetupReason::OptionalToolInvalid)
            } else {
                None
            };
        let mut state = Self {
            status: DesktopStartupStatus::Ready,
            title: "moyAI".to_string(),
            message: "ローカル設定を確認しました。".to_string(),
            detail: "起動時に provider や Docling への network request は送信しません。"
                .to_string(),
            action_overlay: None,
            checks,
            global_config_path,
            setup_generation: 1,
            initial_setup_reason,
            setup_completion_pending: initial_setup_reason.is_some(),
        };
        state.recompute();
        state
    }

    pub fn refresh_config(&mut self, config: &ResolvedConfig) {
        self.setup_generation = self.setup_generation.saturating_add(1);
        self.set_check(Self::provider_config_check(config));
        self.set_check(Self::docling_config_check(config));
        if let Some(reason) = self.current_validation_reason() {
            self.setup_completion_pending = true;
            self.initial_setup_reason = Some(reason);
        }
        self.recompute();
    }

    pub fn complete_after_persist(&mut self) {
        self.setup_generation = self.setup_generation.saturating_add(1);
        if let Some(reason) = self.current_validation_reason() {
            self.setup_completion_pending = true;
            self.initial_setup_reason = Some(reason);
        } else {
            self.setup_completion_pending = false;
            self.initial_setup_reason = None;
        }
        self.recompute();
    }

    pub fn requires_initial_setup(&self) -> bool {
        self.setup_completion_pending
    }

    fn set_check(&mut self, check: DesktopStartupCheck) {
        if let Some(existing) = self.checks.iter_mut().find(|item| item.key == check.key) {
            *existing = check;
        } else {
            self.checks.push(check);
        }
    }

    fn provider_config_check(config: &ResolvedConfig) -> DesktopStartupCheck {
        if config.device_network.configured() && config.device_network.validate().is_ok() {
            return DesktopStartupCheck::pass(
                "provider",
                "Hub設定",
                "Hubの共通設定を確認しました。端末連携から参加してください。モデルの接続はまだ確認していません。",
            );
        }
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

    fn current_validation_reason(&self) -> Option<DesktopInitialSetupReason> {
        if self
            .checks
            .iter()
            .any(|check| check.key == "provider" && check.status == DesktopStartupCheckStatus::Fail)
        {
            Some(DesktopInitialSetupReason::ProviderInvalid)
        } else if self
            .checks
            .iter()
            .any(|check| check.key == "docling" && check.status == DesktopStartupCheckStatus::Fail)
        {
            Some(DesktopInitialSetupReason::OptionalToolInvalid)
        } else {
            None
        }
    }

    fn recompute(&mut self) {
        if self.setup_completion_pending {
            let reason = self
                .current_validation_reason()
                .or(self.initial_setup_reason)
                .unwrap_or(DesktopInitialSetupReason::ConfigMissing);
            self.initial_setup_reason = Some(reason);
            match reason {
                DesktopInitialSetupReason::ConfigMissing => {
                    self.status = DesktopStartupStatus::RequiresConfig;
                    self.title = "設定の確認が必要です".to_string();
                    self.message =
                        "初回起動用の設定を作成しました。初期設定を完了してください。".to_string();
                    self.detail =
                        "LLM、権限、任意ツールを確認して設定ファイルへ保存します。".to_string();
                }
                DesktopInitialSetupReason::ProviderInvalid => {
                    self.status = DesktopStartupStatus::RequiresProvider;
                    self.title = "LLM 設定の確認が必要です".to_string();
                    self.message = "初期設定で LLM URL と model を確認してください。".to_string();
                    self.detail = "外部接続の成功は保存の前提ではありません。".to_string();
                }
                DesktopInitialSetupReason::OptionalToolInvalid => {
                    self.status = DesktopStartupStatus::RequiresConfig;
                    self.title = "Docling 設定の確認が必要です".to_string();
                    self.message = "初期設定で任意ツールの設定を確認してください。".to_string();
                    self.detail =
                        "Doclingを無効にするか、有効なbase URLを入力してください。".to_string();
                }
            }
            self.action_overlay = Some(DesktopOverlay::InitialSetup);
            return;
        }

        self.status = DesktopStartupStatus::Ready;
        self.title = "moyAI".to_string();
        self.message = "ローカル設定を確認しました。".to_string();
        self.detail =
            "provider catalogとavailability diagnosticsは明示操作時だけnetworkへ接続します。"
                .to_string();
        self.action_overlay = None;
        self.initial_setup_reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_hub_config_allows_enrollment_without_a_direct_provider() {
        let mut config = ResolvedConfig::default();
        config.model.base_url.clear();
        config.model.model.clear();
        config.docling.enabled = false;
        config.device_network.hub_url = "https://127.0.0.1:9471".into();
        config.device_network.ca_certificate_pem =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])
                .unwrap()
                .cert
                .pem();
        let state = DesktopStartupState::begin(true, None, Utf8Path::new("."), &config);
        assert!(!state.requires_initial_setup());
        assert_eq!(state.status, DesktopStartupStatus::Ready);
        config.device_network.ca_certificate_pem = "invalid".into();
        let invalid = DesktopStartupState::begin(true, None, Utf8Path::new("."), &config);
        assert!(invalid.requires_initial_setup());
        assert_eq!(invalid.status, DesktopStartupStatus::RequiresProvider);
    }

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
    fn config_refresh_keeps_the_initial_setup_latch_until_persisted_finish() {
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

        assert_eq!(state.status, DesktopStartupStatus::RequiresProvider);
        assert!(state.requires_initial_setup());
        assert!(state.checks.iter().all(|check| {
            check.key != "provider" && check.key != "docling"
                || check.status == DesktopStartupCheckStatus::Pass
        }));
        state.complete_after_persist();
        assert_eq!(state.status, DesktopStartupStatus::Ready);
        assert!(!state.requires_initial_setup());
    }

    #[test]
    fn missing_config_and_invalid_local_fields_share_the_dedicated_setup_shell() {
        let mut valid = ResolvedConfig::default();
        let mut missing = DesktopStartupState::begin(
            false,
            Some(Utf8PathBuf::from("C:/config/config.toml")),
            Utf8Path::new("."),
            &valid,
        );

        assert!(missing.requires_initial_setup());
        assert_eq!(missing.action_overlay, Some(DesktopOverlay::InitialSetup));
        assert_eq!(
            missing.initial_setup_reason,
            Some(DesktopInitialSetupReason::ConfigMissing)
        );

        missing.complete_after_persist();
        assert!(!missing.requires_initial_setup());

        valid.model.base_url.clear();
        missing.refresh_config(&valid);
        assert!(missing.requires_initial_setup());
        assert_eq!(missing.action_overlay, Some(DesktopOverlay::InitialSetup));
        assert_eq!(
            missing.initial_setup_reason,
            Some(DesktopInitialSetupReason::ProviderInvalid)
        );
        missing.complete_after_persist();
        assert!(
            missing.requires_initial_setup(),
            "reviewing a missing config must not bypass a current local validation failure"
        );
    }
}
