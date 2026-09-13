//! Operator commands use the authenticated local IPC boundary. Hub principals are separate.
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use ulid::Ulid;

use super::provision::ProvisionTemplate;
use super::{
    RunnerError, RunnerHost,
    shared::{EnvironmentMapping, SharedSettings},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerOperation {
    Status,
    Pause,
    Resume,
    Drain,
    Maintenance {
        until_ms: Option<u64>,
    },
    InstallSettings {
        settings: SharedSettings,
        templates: Vec<ProvisionTemplate>,
    },
    UpdateTemplates {
        templates: Vec<ProvisionTemplate>,
        expected_templates: Vec<ProvisionTemplate>,
    },
    Provision {
        template_id: String,
        environment_id: String,
    },
    ReconcileUnknown {
        attempt_id: String,
        generation: u64,
        reason: String,
        evidence: ReconciliationEvidence,
    },
    InstallAutostart,
    RemoveAutostart,
    LocalSignIn {
        credentials: LocalCredentials,
    },
    LocalSignOut,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalCredentials {
    pub username: String,
    pub password: String,
    pub project_id: String,
}
impl std::fmt::Debug for LocalCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalCredentials")
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReconciliationEvidence {
    ProcessDrain,
    OperatorConfirmedStopped {
        effects_reviewed: bool,
        processes_stopped: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunnerOperationsProjection {
    pub runner_id: Ulid,
    pub mode: String,
    pub state: String,
    pub accepting: bool,
    pub maintenance_until_ms: Option<u64>,
    pub autostart: bool,
    pub templates: Vec<ProvisionTemplate>,
    pub environments: Vec<EnvironmentMapping>,
    pub active_attempts: Vec<super::shared::SharedAttemptProjection>,
    pub unknown_attempts: Vec<super::shared::SharedAttemptProjection>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProvisionMode {
    #[default]
    Available,
    Paused,
    Draining,
    Maintenance,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Installed {
    version: u32,
    pub mode: ProvisionMode,
    pub maintenance_until_ms: Option<u64>,
    pub settings: Option<SharedSettings>,
    pub templates: Vec<ProvisionTemplate>,
    #[serde(default)]
    pub provisions: Vec<super::shared::provisioning::ProvisionDelivery>,
}

impl Default for Installed {
    fn default() -> Self {
        Self {
            version: 1,
            mode: ProvisionMode::Available,
            maintenance_until_ms: None,
            settings: None,
            templates: vec![],
            provisions: vec![],
        }
    }
}

pub(crate) struct OperationsStore {
    path: Utf8PathBuf,
    pub installed: Installed,
}

impl OperationsStore {
    pub(crate) fn open(data: &Utf8Path) -> Result<Self, RunnerError> {
        let path = data.join("runner-operations.json");
        let installed = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(512 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                if bytes.len() > 512 * 1024 {
                    return Err(RunnerError::new("Runner settings exceed 512 KiB"));
                }
                let value: Installed = serde_json::from_slice(&bytes).map_err(error)?;
                if value.version != 1 {
                    return Err(RunnerError::new("Unsupported Runner settings version"));
                }
                value
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Installed::default(),
            Err(value) => return Err(error(value)),
        };
        Ok(Self { path, installed })
    }

    pub(crate) fn update(&mut self, installed: Installed) -> Result<(), RunnerError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| RunnerError::new("Invalid Runner settings path"))?;
        std::fs::create_dir_all(parent).map_err(error)?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        let bytes = serde_json::to_vec_pretty(&installed).map_err(error)?;
        file.write_all(&bytes).map_err(error)?;
        file.as_file().sync_all().map_err(error)?;
        file.persist(&self.path).map_err(error)?;
        self.installed = installed;
        Ok(())
    }
}

fn error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::new(format!("Runner operations unavailable: {error}"))
}
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_millis().min(u128::from(u64::MAX)) as u64)
}

impl RunnerHost {
    pub fn installed_shared_settings(&self) -> Result<Option<SharedSettings>, RunnerError> {
        Ok(self
            .inner
            .operations
            .lock()
            .map_err(error)?
            .installed
            .settings
            .clone())
    }

    pub(crate) fn accepting_new_shared(&self) -> bool {
        self.inner
            .operations
            .lock()
            .is_ok_and(|store| store.installed.mode == ProvisionMode::Available)
    }

    pub(crate) fn operations_projection(&self) -> Result<RunnerOperationsProjection, RunnerError> {
        let store = self.inner.operations.lock().map_err(error)?;
        let state = self.inner.state.lock().map_err(error)?;
        let shared = state.shared_projection.as_ref();
        let attempts = shared
            .map(|value| value.attempts.clone())
            .unwrap_or_default();
        let active = state.runs.values().any(|value| !value.processes_drained);
        let label = if state.closing {
            "stopping"
        } else {
            match store.installed.mode {
                ProvisionMode::Available => "available",
                ProvisionMode::Paused => "paused",
                ProvisionMode::Draining if active || !attempts.is_empty() => "draining",
                ProvisionMode::Draining | ProvisionMode::Maintenance => "maintenance",
            }
        };
        Ok(RunnerOperationsProjection {
            runner_id: self.inner.id,
            mode: if state.shared_mode {
                "shared"
            } else {
                "private"
            }
            .into(),
            state: label.into(),
            accepting: !state.closing
                && store.installed.mode == ProvisionMode::Available
                && shared.is_none_or(|value| value.accepting),
            maintenance_until_ms: store.installed.maintenance_until_ms,
            autostart: super::autostart::installed()?,
            templates: store.installed.templates.clone(),
            environments: store
                .installed
                .settings
                .as_ref()
                .map(|s| s.environments.clone())
                .unwrap_or_default(),
            active_attempts: attempts
                .iter()
                .filter(|value| value.state != "unknown")
                .cloned()
                .collect(),
            unknown_attempts: attempts
                .into_iter()
                .filter(|value| value.state == "unknown")
                .collect(),
            error: shared.and_then(|value| value.error.clone()),
        })
    }

    pub(crate) async fn operate(
        &self,
        operation: RunnerOperation,
    ) -> Result<RunnerOperationsProjection, RunnerError> {
        match operation {
            RunnerOperation::Status => {}
            RunnerOperation::Pause
            | RunnerOperation::Resume
            | RunnerOperation::Drain
            | RunnerOperation::Maintenance { .. } => {
                let mut store = self.inner.operations.lock().map_err(error)?;
                let mut next = store.installed.clone();
                next.maintenance_until_ms = None;
                next.mode = match operation {
                    RunnerOperation::Pause => ProvisionMode::Paused,
                    RunnerOperation::Resume => ProvisionMode::Available,
                    RunnerOperation::Drain => ProvisionMode::Draining,
                    RunnerOperation::Maintenance { until_ms } => {
                        if until_ms.is_some_and(|until| until <= now_ms()) {
                            return Err(RunnerError::new(
                                "Maintenance expiry must be in the future",
                            ));
                        }
                        next.maintenance_until_ms = until_ms;
                        ProvisionMode::Draining
                    }
                    _ => unreachable!(),
                };
                store.update(next)?;
            }
            RunnerOperation::InstallSettings {
                mut settings,
                mut templates,
            } => {
                settings.resolve()?;
                super::provision::validate_templates(&mut templates)?;
                {
                    let state = self.inner.state.lock().map_err(error)?;
                    if state.closing || state.shared_mode || !state.runs.is_empty() {
                        return Err(RunnerError::new(
                            "Install settings before accepting work; restart a drained Runner to change its authority",
                        ));
                    }
                }
                {
                    let mut store = self.inner.operations.lock().map_err(error)?;
                    let mut next = store.installed.clone();
                    next.settings = Some(settings.clone());
                    next.templates = templates;
                    store.update(next)?;
                }
                // The task owns the host until explicit shutdown; dropping this join handle
                // does not cancel already accepted work.
                let _worker = super::shared::SharedWorker::start(self.clone(), settings).await?;
            }
            RunnerOperation::UpdateTemplates {
                mut templates,
                expected_templates,
            } => {
                super::provision::validate_templates(&mut templates)?;
                let mut store = self.inner.operations.lock().map_err(error)?;
                if store.installed.templates != expected_templates {
                    return Err(RunnerError::new(
                        "Provision templates changed; refresh the current local authority before applying this edit",
                    ));
                }
                let mut next = store.installed.clone();
                next.templates = templates;
                store.update(next)?;
            }
            RunnerOperation::InstallAutostart => super::autostart::install()?,
            RunnerOperation::RemoveAutostart => super::autostart::remove()?,
            operation @ (RunnerOperation::Provision { .. }
            | RunnerOperation::ReconcileUnknown { .. }
            | RunnerOperation::LocalSignIn { .. }
            | RunnerOperation::LocalSignOut) => {
                let sender = self
                    .inner
                    .state
                    .lock()
                    .map_err(error)?
                    .shared_commands
                    .clone()
                    .ok_or_else(|| RunnerError::new("Shared Runner is not configured"))?;
                let (reply, receive) = tokio::sync::oneshot::channel();
                sender
                    .try_send(super::shared::OperatorRequest { operation, reply })
                    .map_err(error)?;
                receive.await.map_err(error)??;
            }
        }
        self.operations_projection()
    }

    pub(crate) fn refresh_maintenance(&self) -> Result<(), RunnerError> {
        let mut store = self.inner.operations.lock().map_err(error)?;
        if store
            .installed
            .maintenance_until_ms
            .is_some_and(|until| now_ms() >= until)
        {
            let mut next = store.installed.clone();
            // Expiration ends the scheduled hold, but never overrides explicit Pause/Stop.
            if matches!(
                next.mode,
                ProvisionMode::Draining | ProvisionMode::Maintenance
            ) {
                next.mode = ProvisionMode::Available;
            }
            next.maintenance_until_ms = None;
            store.update(next)?;
        }
        Ok(())
    }
}

/// Launch the adjacent product Runner in the current OS account, without a shell or elevation.
pub fn launch() -> Result<(), RunnerError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let executable = std::env::current_exe()
            .map_err(error)?
            .with_file_name("moyai-runner.exe");
        if !executable.is_file() {
            return Err(RunnerError::new("The adjacent moyai-runner.exe is missing"));
        }
        let paths = crate::storage::StoragePaths::discover().map_err(error)?;
        std::fs::create_dir_all(&paths.data_dir).map_err(error)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.data_dir.join("runner.log"))
            .map_err(error)?;
        std::process::Command::new(executable)
            .args(["serve", "--background"])
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().map_err(error)?)
            .stderr(log)
            .spawn()
            .map_err(error)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new("Runner launch supports Windows only"))
    }
}
