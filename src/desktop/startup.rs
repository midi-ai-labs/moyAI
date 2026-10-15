use camino::{Utf8Path, Utf8PathBuf};

use crate::config::ResolvedConfig;
use crate::docling::normalize_docling_base_url;
use crate::llm::normalize_provider_base_url;

use super::preferences::DesktopOnboardingIntent;
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
    SetupUnfinished,
    ProviderInvalid,
    OptionalToolInvalid,
}

impl DesktopInitialSetupReason {
    pub const fn key(self) -> &'static str {
        match self {
            Self::ConfigMissing => "config_missing",
            Self::SetupUnfinished => "setup_unfinished",
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
    pub onboarding_intent: Option<DesktopOnboardingIntent>,
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
            onboarding_intent: None,
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
            detail:
                "起動時は保存された設定を読み込みます。AIや文書変換サービスへの接続は未確認です。"
                    .to_string(),
            action_overlay: None,
            checks,
            global_config_path,
            setup_generation: 1,
            initial_setup_reason,
            setup_completion_pending: initial_setup_reason.is_some(),
            onboarding_intent: None,
        };
        state.recompute();
        state
    }

    pub fn resume_onboarding(&mut self, intent: DesktopOnboardingIntent) {
        self.onboarding_intent = Some(intent);
        self.setup_completion_pending = true;
        if self.initial_setup_reason.is_none() {
            self.initial_setup_reason = Some(DesktopInitialSetupReason::SetupUnfinished);
        }
        self.setup_generation = self.setup_generation.saturating_add(1);
        self.recompute();
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
            self.onboarding_intent = None;
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
                "Hubの接続設定があります。「PCの接続」から参加してください。AIへの接続は未確認です。",
            );
        }
        let base_url = normalize_provider_base_url(&config.model.base_url);
        let model = config.model.model.trim();
        if base_url.is_empty() {
            return DesktopStartupCheck::fail(
                "provider",
                "AIの設定",
                "AIの接続先URLが未設定です。",
            );
        }
        if model.is_empty() {
            return DesktopStartupCheck::fail("provider", "AIの設定", "モデルが未設定です。");
        }
        DesktopStartupCheck::pass(
            "provider",
            "AIの設定",
            format!(
                "設定値あり（接続は未確認）: {base_url} / {model}。モデル一覧の取得、または依頼の送信時に接続します。"
            ),
        )
    }

    fn docling_config_check(config: &ResolvedConfig) -> DesktopStartupCheck {
        if !config.docling.enabled {
            return DesktopStartupCheck::pass(
                "docling",
                "Docling 設定",
                "無効です。PDF・Wordなどの文書を変換する場合は、設定から有効にしてください。",
            );
        }
        let base_url = normalize_docling_base_url(&config.docling.base_url);
        if base_url.is_empty() {
            return DesktopStartupCheck::fail(
                "docling",
                "Docling 設定",
                "Doclingの接続先URLが未設定です。",
            );
        }
        DesktopStartupCheck::pass(
            "docling",
            "Docling 設定",
            format!("設定値あり（接続は未確認）: {base_url}。文書の変換時に接続します。"),
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
                DesktopInitialSetupReason::SetupUnfinished => {
                    self.status = DesktopStartupStatus::RequiresConfig;
                    self.title = "初回設定を再開します".to_string();
                    self.message = "前回選んだ使い方から設定を再開できます。".to_string();
                    self.detail = "画面に沿って入力し、最後に設定を保存してください。".to_string();
                }
                DesktopInitialSetupReason::ConfigMissing => {
                    self.status = DesktopStartupStatus::RequiresConfig;
                    self.title = "設定の確認が必要です".to_string();
                    self.message =
                        "初回起動用の設定を作成しました。初期設定を完了してください。".to_string();
                    self.detail = "使い方を選び、必要な設定を保存してください。".to_string();
                }
                DesktopInitialSetupReason::ProviderInvalid => {
                    self.status = DesktopStartupStatus::RequiresProvider;
                    self.title = "AIの接続設定を修正してください".to_string();
                    self.message =
                        "初回設定でAIの接続先URLとモデルを確認してください。".to_string();
                    self.detail = "まだ接続できなくても設定は保存できます。".to_string();
                }
                DesktopInitialSetupReason::OptionalToolInvalid => {
                    self.status = DesktopStartupStatus::RequiresConfig;
                    self.title = "Docling 設定の確認が必要です".to_string();
                    self.message = "初期設定で任意ツールの設定を確認してください。".to_string();
                    self.detail =
                        "Doclingを無効にするか、有効な接続先URLを入力してください。".to_string();
                }
            }
            self.action_overlay = Some(DesktopOverlay::InitialSetup);
            return;
        }

        self.status = DesktopStartupStatus::Ready;
        self.title = "moyAI".to_string();
        self.message = "ローカル設定を確認しました。".to_string();
        self.detail =
            "モデル一覧の取得や接続テストは、必要なときに設定画面から実行できます。".to_string();
        self.action_overlay = None;
        self.initial_setup_reason = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_setup_remains_pending_after_default_config_exists() {
        let config = ResolvedConfig::default();
        let mut restarted = DesktopStartupState::begin(true, None, Utf8Path::new("."), &config);
        assert!(
            !restarted.requires_initial_setup(),
            "file presence alone loses unfinished setup"
        );
        restarted.resume_onboarding(DesktopOnboardingIntent::Personal);
        assert!(restarted.requires_initial_setup());
        assert_eq!(
            restarted.onboarding_intent,
            Some(DesktopOnboardingIntent::Personal)
        );
        restarted.refresh_config(&config);
        assert!(restarted.requires_initial_setup());
        restarted.complete_after_persist();
        assert!(!restarted.requires_initial_setup());
        assert_eq!(restarted.onboarding_intent, None);
    }

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
