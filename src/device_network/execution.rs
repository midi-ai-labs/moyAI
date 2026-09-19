//! Desktop manages the existing local execution host; installed Runner settings own consent.
use super::{DeviceClient, DeviceNetworkService};
use crate::config::AccessMode;
use crate::runner::{
    RunnerCommand, RunnerResponse,
    operations::{
        OperationsStore, ReconciliationEvidence, RunnerOperation, RunnerOperationsProjection,
    },
    provision::{DESKTOP_TEMPLATE_ID, ProvisionTemplate},
    shared::{ResourceScope, SharedAttemptProjection, SharedSettings},
};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceProject {
    pub id: String,
    pub label: String,
    pub can_control: bool,
    pub can_execute: bool,
    pub environment_id: Option<String>,
    pub preparation_state: String,
    pub error: Option<String>,
}
#[derive(Deserialize)]
struct DeviceProjects {
    projects: Vec<DeviceProject>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceExecutionState {
    #[default]
    Unconnected,
    NotSelected,
    NeedsSetup,
    Starting,
    Ready,
    Paused,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionReview {
    pub id: String,
    pub directory: Utf8PathBuf,
    pub access_mode: AccessMode,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceExecutionProjection {
    #[cfg(feature = "desktop-e2e")]
    pub isolated_test_host: bool,
    pub revision: String,
    pub state: DeviceExecutionState,
    pub projects: Vec<DeviceProject>,
    pub review: Option<ExecutionReview>,
    pub directory: Option<Utf8PathBuf>,
    pub access_mode: Option<AccessMode>,
    pub accepting: bool,
    pub can_pause: bool,
    pub can_resume: bool,
    pub unknown_attempts: Vec<SharedAttemptProjection>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceExecutionCommand {
    Prepare {
        access_mode: AccessMode,
    },
    Enable {
        review_id: String,
    },
    Pause,
    Resume,
    Reconcile {
        attempt_id: String,
        generation: u64,
        reason: String,
        evidence: ReconciliationEvidence,
    },
}
#[derive(Default)]
pub(super) struct ExecutionOwner {
    started: AtomicBool,
    lane: tokio::sync::Mutex<()>,
    state: Mutex<ExecutionRuntime>,
}
#[derive(Default)]
struct ExecutionRuntime {
    revision: u64,
    binding: String,
    view: DeviceExecutionProjection,
}
impl ExecutionRuntime {
    fn revise(&mut self, mut view: DeviceExecutionProjection) {
        view.revision = self.view.revision.clone();
        if self.view != view {
            self.revision += 1;
            view.revision = self.revision.to_string();
            self.view = view;
        }
    }
    fn bind(&mut self, binding: &str) {
        if self.binding != binding {
            self.binding = binding.into();
            self.revise(DeviceExecutionProjection::default());
        }
    }
    fn refresh_projects(&mut self, projects: Vec<DeviceProject>, consented: bool) {
        let mut view = self.view.clone();
        view.projects = projects;
        if !consented {
            view.accepting = false;
            view.can_pause = false;
            view.can_resume = false;
            view.unknown_attempts.clear();
            // A project poll cannot confirm that the pending native setup succeeded.
            // Keep its failure visible alongside the review until retry or replacement.
            if view.review.is_none() {
                view.error = None;
            }
            view.directory = None;
            view.access_mode = None;
            view.state = if view.error.is_some() {
                DeviceExecutionState::Unavailable
            } else if view.projects.iter().any(|project| project.can_execute) {
                DeviceExecutionState::NeedsSetup
            } else {
                DeviceExecutionState::NotSelected
            };
        } else if matches!(
            view.state,
            DeviceExecutionState::Unconnected | DeviceExecutionState::NeedsSetup
        ) {
            view.state = DeviceExecutionState::Starting;
        }
        self.revise(view);
    }
}
struct Connection {
    client: DeviceClient,
    hub_id: String,
    binding: String,
}

impl DeviceNetworkService {
    /// Desktop calls this once. Runner transport never recursively manages another Runner.
    pub fn start_execution_management(&self) {
        if self.inner.execution.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = self.downgrade();
        tokio::spawn(async move {
            loop {
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if service.inner.state.lock().unwrap().closing {
                    break;
                }
                if let Ok(_lane) = service.inner.execution.lane.try_lock() {
                    if let Err(error) = service.refresh_execution().await {
                        service.execution_error(error);
                    }
                }
                drop(service);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }
    pub fn execution_projection(&self) -> DeviceExecutionProjection {
        let mut value = self.inner.execution.state.lock().unwrap().view.clone();
        #[cfg(feature = "desktop-e2e")]
        {
            value.isolated_test_host = std::env::var_os("MOYAI_DESKTOP_E2E_RUNNER").is_some()
                && std::env::var_os("MOYAI_TEST_RESOURCE_REGISTRY").is_some();
        }
        if value.revision.is_empty() {
            value.revision = "0".into();
        }
        value
    }
    fn execution_error(&self, message: String) {
        let mut state = self.inner.execution.state.lock().unwrap();
        let mut view = state.view.clone();
        view.state = DeviceExecutionState::Unavailable;
        view.accepting = false;
        view.can_pause = false;
        view.can_resume = false;
        view.error = Some(message);
        state.revise(view);
    }
    fn execution_connection(&self) -> Option<Connection> {
        let state = self.inner.state.lock().ok()?;
        if state.closing || state.status != "active" {
            return None;
        }
        let hub_id = state.settings.hub_id.clone()?;
        let client = state.client.clone()?;
        let binding = format!(
            "{}|{}|{}|{:x}",
            hub_id,
            client.device_id,
            client.endpoint(),
            Sha256::digest(state.shared.ca_certificate_pem.as_bytes())
        );
        Some(Connection {
            client,
            hub_id,
            binding,
        })
    }
    fn require_execution_binding(&self, binding: &str) -> Result<(), String> {
        if self
            .execution_connection()
            .is_none_or(|current| current.binding != binding)
        {
            Err("Hubまたは端末の接続が変わりました。実行場所を選び直してください。".into())
        } else {
            Ok(())
        }
    }
    async fn refresh_execution(&self) -> Result<(), String> {
        let Some(connection) = self.execution_connection() else {
            let mut state = self.inner.execution.state.lock().unwrap();
            state.bind("");
            state.revise(DeviceExecutionProjection::default());
            return Ok(());
        };
        let directory = self.inner.store.paths().data_dir.clone();
        let installed = tokio::task::spawn_blocking(move || {
            OperationsStore::open(&directory).map(|store| store.installed)
        })
        .await
        .map_err(|_| "実行設定を読み込めません。".to_string())?
        .map_err(|e| e.message)?;
        let projects: DeviceProjects = connection
            .client
            .request("/v1/shared/device-projects", None)
            .await
            .map_err(|e| e.to_string())?;
        if projects.projects.len() > 1024 {
            return Err("PCのプロジェクト設定が上限を超えています。".into());
        }
        self.require_execution_binding(&connection.binding)?;
        {
            let mut state = self.inner.execution.state.lock().unwrap();
            state.bind(&connection.binding);
            let consented =
                installed.desktop_binding.as_deref() == Some(connection.binding.as_str());
            state.refresh_projects(projects.projects, consented);
            if !consented {
                return Ok(());
            }
        }
        let mut status = ensure_status().await?;
        self.require_execution_binding(&connection.binding)?;
        if status.desktop_binding.as_deref() != Some(&connection.binding) {
            return Err("起動中の実行機能と、保存済みのPC設定が一致しません。".into());
        }
        // Consent may already be durable when the first worker startup failed.
        // Retry that same installation only; no accepted execution is replayed.
        if status.mode == "private" {
            let mut settings = installed
                .settings
                .ok_or_else(|| "保存済み実行設定がありません。".to_string())?;
            if !settings.environments.is_empty() {
                return Err("実行機能を再起動して保存済みの環境を復元してください。".into());
            }
            settings.environments.clear();
            let template = installed
                .templates
                .into_iter()
                .find(|value| value.id == DESKTOP_TEMPLATE_ID)
                .ok_or_else(|| "保存済み実行場所がありません。".to_string())?;
            status = request_operation(
                status.runner_id,
                RunnerOperation::InstallDesktop {
                    settings,
                    template,
                    binding: connection.binding.clone(),
                },
            )
            .await?;
            self.require_execution_binding(&connection.binding)?;
        }
        self.accept_execution_status(&connection.binding, status);
        Ok(())
    }
    fn accept_execution_status(&self, binding: &str, status: RunnerOperationsProjection) {
        let mut state = self.inner.execution.state.lock().unwrap();
        if state.binding != binding {
            return;
        }
        let mut view = state.view.clone();
        if let Some(template) = status
            .templates
            .iter()
            .find(|value| value.id == DESKTOP_TEMPLATE_ID)
        {
            view.directory = Some(template.base_root.clone());
            view.access_mode = Some(template.access_mode);
        }
        view.accepting = status.accepting;
        view.can_pause = status.mode == "shared" && status.state == "available";
        view.can_resume = status.mode == "shared"
            && matches!(status.state.as_str(), "paused" | "draining" | "maintenance");
        view.unknown_attempts = status.unknown_attempts;
        view.error = status.error;
        view.state = if view.error.is_some() {
            DeviceExecutionState::Unavailable
        } else if status.state != "available" {
            DeviceExecutionState::Paused
        } else if !view.projects.iter().any(|project| project.can_execute) {
            DeviceExecutionState::NotSelected
        } else {
            DeviceExecutionState::Ready
        };
        state.revise(view);
    }
    pub async fn execution_command(
        &self,
        expected_revision: &str,
        command: DeviceExecutionCommand,
    ) -> DeviceExecutionProjection {
        let _lane = self.inner.execution.lane.lock().await;
        if self.execution_projection().revision != expected_revision {
            self.execution_error("PCの状態が更新されました。現在の設定を確認してください。".into());
            return self.execution_projection();
        }
        if let Err(error) = self.execute_execution_command(command).await {
            self.execution_error(error);
        }
        self.execution_projection()
    }
    async fn execute_execution_command(
        &self,
        command: DeviceExecutionCommand,
    ) -> Result<(), String> {
        let connection = self
            .execution_connection()
            .ok_or_else(|| "先にこのPCのHub参加を完了してください。".to_string())?;
        match command {
            DeviceExecutionCommand::Prepare { access_mode } => {
                let Some(directory) = choose_execution_root().await? else {
                    return Ok(());
                };
                self.require_execution_binding(&connection.binding)?;
                let mut state = self.inner.execution.state.lock().unwrap();
                state.bind(&connection.binding);
                let mut view = state.view.clone();
                view.review = Some(ExecutionReview {
                    id: ulid::Ulid::new().to_string(),
                    directory,
                    access_mode,
                });
                view.error = None;
                state.revise(view);
            }
            DeviceExecutionCommand::Enable { review_id } => {
                let review = {
                    let state = self.inner.execution.state.lock().unwrap();
                    if state.binding != connection.binding {
                        return Err("PCの接続が変わりました。実行場所を選び直してください。".into());
                    }
                    state
                        .view
                        .review
                        .clone()
                        .filter(|review| review.id == review_id)
                        .ok_or_else(|| {
                            "確認対象の実行設定が変わりました。実行場所を選び直してください。"
                                .to_string()
                        })?
                };
                let status = ensure_status().await?;
                self.require_execution_binding(&connection.binding)?;
                let operation = RunnerOperation::InstallDesktop {
                    settings: SharedSettings {
                        version: 1,
                        hub_id: connection.hub_id,
                        device_id: connection.client.device_id,
                        environments: vec![],
                        resource_scope: ResourceScope::Device,
                    },
                    template: ProvisionTemplate {
                        id: DESKTOP_TEMPLATE_ID.into(),
                        label: "このPCの実行設定".into(),
                        base_root: review.directory,
                        access_mode: review.access_mode,
                        allowed_child_environments: vec![],
                    },
                    binding: connection.binding.clone(),
                };
                let updated = request_operation(status.runner_id, operation).await?;
                self.require_execution_binding(&connection.binding)?;
                self.accept_execution_status(&connection.binding, updated);
                let mut state = self.inner.execution.state.lock().unwrap();
                let mut view = state.view.clone();
                view.review = None;
                state.revise(view);
            }
            DeviceExecutionCommand::Pause
            | DeviceExecutionCommand::Resume
            | DeviceExecutionCommand::Reconcile { .. } => {
                let status = ensure_status().await?;
                self.require_execution_binding(&connection.binding)?;
                if status.desktop_binding.as_deref() != Some(connection.binding.as_str()) {
                    return Err("現在のHubに対する実行許可を確認できません。".into());
                }
                let operation = match command {
                    DeviceExecutionCommand::Pause => RunnerOperation::Pause,
                    DeviceExecutionCommand::Resume => RunnerOperation::Resume,
                    DeviceExecutionCommand::Reconcile {
                        attempt_id,
                        generation,
                        reason,
                        evidence,
                    } => {
                        if !status.unknown_attempts.iter().any(|attempt| {
                            attempt.attempt_id == attempt_id && attempt.generation == generation
                        }) {
                            return Err(
                                "確認対象の実行が変わりました。現在の停止状況を確認してください。"
                                    .into(),
                            );
                        }
                        RunnerOperation::ReconcileUnknown {
                            attempt_id,
                            generation,
                            reason,
                            evidence,
                        }
                    }
                    _ => unreachable!(),
                };
                let status = request_operation(status.runner_id, operation).await?;
                self.require_execution_binding(&connection.binding)?;
                self.accept_execution_status(&connection.binding, status);
            }
        }
        Ok(())
    }
}

async fn ensure_status() -> Result<RunnerOperationsProjection, String> {
    let identity = tokio::task::spawn_blocking(crate::runner::operations::ensure_started)
        .await
        .map_err(|_| "実行機能の起動を確認できません。".to_string())?
        .map_err(|e| e.message)?;
    request_operation(identity.runner_id, RunnerOperation::Status).await
}
async fn request_operation(
    runner_id: ulid::Ulid,
    operation: RunnerOperation,
) -> Result<RunnerOperationsProjection, String> {
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        let response = crate::runner::windows::request(&RunnerCommand::Operations {
            runner_id,
            operation,
        })
        .map_err(|e| e.message)?;
        #[cfg(not(windows))]
        let response: RunnerResponse = {
            let _ = (runner_id, operation);
            return Err("実行機能はWindowsで使用してください。".into());
        };
        match response {
            RunnerResponse::Operations { projection } => Ok(projection),
            _ => Err("実行機能からの応答が不正です。".into()),
        }
    })
    .await
    .map_err(|_| "実行機能との通信を確認できません。".to_string())?
}
async fn choose_execution_root() -> Result<Option<Utf8PathBuf>, String> {
    #[cfg(feature = "tauri-desktop")]
    {
        tokio::task::spawn_blocking(|| {
            rfd::FileDialog::new()
                .set_title("このPCで仕事を実行する場所")
                .pick_folder()
                .map(Utf8PathBuf::from_path_buf)
                .transpose()
                .map_err(|_| "UTF-8の実行場所を選んでください。".to_string())
        })
        .await
        .map_err(|_| "フォルダを選択できません。".to_string())?
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        Err("実行場所はDesktopから選択してください。".into())
    }
}
