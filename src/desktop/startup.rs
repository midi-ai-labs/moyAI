use camino::{Utf8Path, Utf8PathBuf};

use crate::config::ResolvedConfig;
use crate::docling::normalize_docling_base_url;
use crate::llm::{ModelAvailabilityReport, ModelAvailabilityStatus, normalize_provider_base_url};

use super::state::DesktopOverlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopStartupStatus {
    Loading,
    Ready,
    RequiresConfig,
    RequiresProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopStartupCheckStatus {
    Pending,
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
    fn pending(key: &'static str, label: &'static str, message: impl Into<String>) -> Self {
        Self {
            key,
            label,
            status: DesktopStartupCheckStatus::Pending,
            message: message.into(),
        }
    }

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

        let base_url = normalize_provider_base_url(&config.model.base_url);
        let model = config.model.model.trim();
        checks.push(if base_url.is_empty() {
            DesktopStartupCheck::fail("provider", "LLM 接続", "LLM URL が未設定です。")
        } else if model.is_empty() {
            DesktopStartupCheck::fail("provider", "LLM 接続", "model が未設定です。")
        } else {
            DesktopStartupCheck::pending(
                "provider",
                "LLM 接続",
                format!("{base_url} / {model} を確認しています。"),
            )
        });

        checks.push(Self::docling_check_for_config(config));

        let mut state = Self {
            status: DesktopStartupStatus::Loading,
            title: "moyAI を準備しています".to_string(),
            message: "ローカル設定、LLM 接続、補助サービスを確認しています。".to_string(),
            detail: "この確認は閉域・ローカル LLM 環境内で完結します。".to_string(),
            action_overlay: None,
            checks,
            config_requires_attention: !global_config_existed_at_launch,
        };
        state.recompute();
        state
    }

    pub fn complete_model_availability(&mut self, report: &ModelAvailabilityReport) {
        if report.model.trim().is_empty() {
            self.set_provider_check(DesktopStartupCheck::fail(
                "provider",
                "LLM 接続",
                "model が未設定です。",
            ));
            self.recompute();
            return;
        }
        if matches!(report.status, ModelAvailabilityStatus::Pass) {
            let source = report
                .matched_model
                .as_ref()
                .map(|model| {
                    if model.loaded {
                        format!("{} / loaded", model.source)
                    } else {
                        model.source.clone()
                    }
                })
                .unwrap_or_else(|| "model availability report".to_string());
            self.set_provider_check(DesktopStartupCheck::pass(
                "provider",
                "LLM 接続",
                format!("接続できました: {} ({source})", report.model),
            ));
        } else {
            let failure = report
                .tool_call_probes
                .iter()
                .find_map(|probe| probe.error.as_deref())
                .or_else(|| {
                    report
                        .vision_probes
                        .iter()
                        .find_map(|probe| probe.error.as_deref())
                })
                .or(report.openai_error.as_deref())
                .or(report.native_error.as_deref())
                .unwrap_or("model availability gate failed");
            self.set_provider_check(DesktopStartupCheck::fail(
                "provider",
                "LLM 接続",
                format!(
                    "設定中の model `{}` を利用できません: {failure}",
                    report.model
                ),
            ));
        }
        self.recompute();
    }

    pub fn begin_provider_availability(&mut self, base_url: &str, model: &str) {
        let base_url = normalize_provider_base_url(base_url);
        let model = model.trim();
        if base_url.is_empty() {
            self.set_provider_check(DesktopStartupCheck::fail(
                "provider",
                "LLM 接続",
                "LLM URL が未設定です。",
            ));
        } else if model.is_empty() {
            self.set_provider_check(DesktopStartupCheck::fail(
                "provider",
                "LLM 接続",
                "model が未設定です。",
            ));
        } else {
            self.set_provider_check(DesktopStartupCheck::pending(
                "provider",
                "LLM 接続",
                format!("{base_url} / {model} を確認しています。"),
            ));
        }
        self.recompute();
    }

    pub fn fail_provider_availability(&mut self, message: impl Into<String>) {
        self.set_provider_check(DesktopStartupCheck::fail(
            "provider",
            "LLM 接続",
            message.into(),
        ));
        self.recompute();
    }

    pub fn begin_docling_check(&mut self, config: &ResolvedConfig) -> bool {
        let check = Self::docling_check_for_config(config);
        let should_probe = check.status == DesktopStartupCheckStatus::Pending;
        self.set_check(check);
        self.recompute();
        should_probe
    }

    pub fn complete_docling_check(&mut self, base_url: &str) {
        self.set_check(DesktopStartupCheck::pass(
            "docling",
            "Docling 接続",
            format!("接続できました: {}", normalize_docling_base_url(base_url)),
        ));
        self.recompute();
    }

    pub fn fail_docling_check(&mut self, message: impl Into<String>) {
        self.set_check(DesktopStartupCheck::fail(
            "docling",
            "Docling 接続",
            message.into(),
        ));
        self.recompute();
    }

    pub fn mark_config_reviewed(&mut self) {
        self.config_requires_attention = false;
        self.recompute();
    }

    pub fn requires_initial_setup(&self) -> bool {
        self.config_requires_attention
    }

    fn set_provider_check(&mut self, check: DesktopStartupCheck) {
        self.set_check(check);
    }

    fn set_check(&mut self, check: DesktopStartupCheck) {
        if let Some(existing) = self.checks.iter_mut().find(|item| item.key == check.key) {
            *existing = check;
        } else {
            self.checks.push(check);
        }
    }

    fn docling_check_for_config(config: &ResolvedConfig) -> DesktopStartupCheck {
        if !config.docling.enabled {
            return DesktopStartupCheck::pass(
                "docling",
                "Docling 接続",
                "無効です。structured document 処理が必要な場合は設定から有効化してください。",
            );
        }
        let base_url = normalize_docling_base_url(&config.docling.base_url);
        if base_url.is_empty() {
            return DesktopStartupCheck::fail(
                "docling",
                "Docling 接続",
                "Docling Serve URL が未設定です。",
            );
        }
        DesktopStartupCheck::pending(
            "docling",
            "Docling 接続",
            format!("{base_url} の /health と /ready を確認しています。"),
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
        let startup_pending = self
            .checks
            .iter()
            .any(|check| check.status == DesktopStartupCheckStatus::Pending);

        if startup_pending {
            let pending_labels = self
                .checks
                .iter()
                .filter(|check| check.status == DesktopStartupCheckStatus::Pending)
                .map(|check| check.label)
                .collect::<Vec<_>>()
                .join(" / ");
            self.status = DesktopStartupStatus::Loading;
            self.title = "moyAI を準備しています".to_string();
            self.message = format!("{pending_labels} を確認しています。");
            self.action_overlay = None;
            return;
        }

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
            self.title = "LLM 接続の確認が必要です".to_string();
            self.message = "起動後に LLM URL 画面を開きます。".to_string();
            self.detail = "provider を起動し、base URL と model を確認してください。".to_string();
            self.action_overlay = Some(DesktopOverlay::ProviderEditor);
            return;
        }

        if docling_failed {
            self.status = DesktopStartupStatus::RequiresConfig;
            self.title = "Docling 接続の確認が必要です".to_string();
            self.message = "起動後に設定画面を開きます。".to_string();
            self.detail =
                "Docling Serve を起動し、docling.enabled と docling.base_url を確認してください。"
                    .to_string();
            self.action_overlay = Some(DesktopOverlay::ConfigEditor);
            return;
        }

        self.status = DesktopStartupStatus::Ready;
        self.title = "moyAI".to_string();
        self.message = "起動準備が完了しました。".to_string();
        self.detail = "メインウィンドウを表示します。".to_string();
        self.action_overlay = None;
    }
}

#[cfg(test)]
mod tests {}
