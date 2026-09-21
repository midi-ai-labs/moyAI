//! Operator commands use the authenticated local IPC boundary. Hub principals are separate.
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use ulid::Ulid;

#[cfg(test)]
mod tests;

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
    QuiescentShutdown {
        expected_desktop_binding: String,
    },
    Maintenance {
        until_ms: Option<u64>,
    },
    InstallSettings {
        settings: SharedSettings,
        templates: Vec<ProvisionTemplate>,
    },
    InstallDesktop {
        settings: SharedSettings,
        template: ProvisionTemplate,
        binding: String,
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
    LocalProject {
        project_id: Option<String>,
    },
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
    pub desktop_binding: Option<String>,
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
    #[serde(default)]
    pub desktop_binding: Option<String>,
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
            desktop_binding: None,
        }
    }
}

/// Saved execution consent follows the authenticated Hub/device trust identity.
/// Transient reviews and async targets continue to compare the complete binding.
pub(crate) fn desktop_consent_matches(saved: &str, current: &str) -> bool {
    fn identity(value: &str) -> Option<(&str, &str, &str)> {
        let parts = value.split('|').collect::<Vec<_>>();
        if parts.len() != 4
            || !crate::device_network::stable_id(parts[0])
            || !crate::device_network::stable_id(parts[1])
            || parts[3].len() != 64
            || !parts[3]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let url = reqwest::Url::parse(parts[2]).ok()?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        Some((parts[0], parts[1], parts[3]))
    }
    matches!((identity(saved), identity(current)), (Some(a), Some(b)) if a == b)
}
impl Installed {
    pub(crate) fn child_environments(
        &self,
        mapping: &EnvironmentMapping,
        hub_allowed: &[String],
    ) -> Vec<String> {
        if self.desktop_binding.is_some()
            && self
                .provisions
                .iter()
                .any(|receipt| receipt.desktop_environment(&mapping.environment_id))
        {
            // One-time Desktop consent covers delegation only within current Hub project authority.
            hub_allowed.to_vec()
        } else {
            mapping
                .allowed_child_environments
                .iter()
                .filter(|id| hub_allowed.contains(id))
                .cloned()
                .collect()
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
        let settings = self
            .inner
            .operations
            .lock()
            .map_err(error)?
            .installed
            .settings
            .clone();
        if let Some(settings) = &settings {
            if crate::device_network::reset::configured_execution_reset(
                &settings.hub_id,
                &settings.device_id,
            )
            .map_err(error)?
            {
                return Ok(None);
            }
        }
        Ok(settings)
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
            desktop_binding: store.installed.desktop_binding.clone(),
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
                if self.inner.state.lock().map_err(error)?.closing {
                    return Err(RunnerError::new(
                        "Runner is stopping; its saved acceptance mode cannot change",
                    ));
                }
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
            RunnerOperation::InstallDesktop {
                mut settings,
                template,
                binding,
            } => {
                if binding.is_empty()
                    || binding.len() > 4096
                    || !matches!(
                        settings.resource_scope,
                        super::shared::ResourceScope::Device
                    )
                    || !settings.environments.is_empty()
                    || template.id != super::provision::DESKTOP_TEMPLATE_ID
                    || !template.allowed_child_environments.is_empty()
                {
                    return Err(RunnerError::new("Invalid Desktop execution consent"));
                }
                settings.resolve()?;
                let mut templates = vec![template];
                super::provision::validate_templates(&mut templates)?;
                let shared = {
                    let state = self.inner.state.lock().map_err(error)?;
                    if state.closing || (!state.shared_mode && !state.runs.is_empty()) {
                        return Err(RunnerError::new(
                            "Wait for local execution to finish before enabling this PC",
                        ));
                    }
                    state.shared_mode
                };
                {
                    let mut store = self.inner.operations.lock().map_err(error)?;
                    let mut next = store.installed.clone();
                    if shared {
                        let current = next.settings.as_ref().ok_or_else(|| {
                            RunnerError::new("Execution settings are unavailable")
                        })?;
                        if current.hub_id != settings.hub_id
                            || current.device_id != settings.device_id
                            || current.resource_scope != settings.resource_scope
                        {
                            return Err(RunnerError::new(
                                "Existing execution authority belongs to another Hub, device, or resource scope",
                            ));
                        }
                    } else {
                        next.settings = Some(settings.clone());
                        // New local consent starts fresh authority. Old provisioning
                        // receipts and pause policy must not follow a retired Hub.
                        next.provisions.clear();
                        next.templates.clear();
                        next.mode = ProvisionMode::Available;
                        next.maintenance_until_ms = None;
                    }
                    next.templates
                        .retain(|value| value.id != super::provision::DESKTOP_TEMPLATE_ID);
                    next.templates.extend(templates);
                    next.desktop_binding = Some(binding);
                    store.update(next)?;
                }
                if !shared {
                    let _worker =
                        super::shared::SharedWorker::start(self.clone(), settings).await?;
                }
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
            | RunnerOperation::QuiescentShutdown { .. }
            | RunnerOperation::ReconcileUnknown { .. }
            | RunnerOperation::LocalProject { .. }) => {
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

/// Explicit Desktop recovery can remove the current user's login hook without
/// requiring the old Hub or Runner IPC to answer.
pub fn remove_autostart_after_reset() -> Result<(), RunnerError> {
    super::autostart::remove_adjacent_runner()
}

/// Launch the adjacent product Runner in the current OS account, without a shell or elevation.
pub fn launch() -> Result<(), RunnerError> {
    launch_process().map(|_| ())
}

fn launch_process() -> Result<std::process::Child, RunnerError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let executable = std::env::current_exe()
            .map_err(error)?
            .with_file_name("moyai-runner.exe");
        #[cfg(feature = "desktop-e2e")]
        let fixture = std::env::var_os("MOYAI_DESKTOP_E2E_RUNNER");
        #[cfg(feature = "desktop-e2e")]
        let fixture_registry = if let Some(instance) =
            crate::desktop_test::current().map_err(error)?
        {
            if fixture.is_none() {
                return Err(RunnerError::new(
                    "An isolated Desktop must use its dedicated E2E Runner; normal machine startup is not permitted",
                ));
            }
            Some(instance.resource_directory())
        } else if fixture.is_some() {
            Some(
                std::env::var_os("MOYAI_TEST_RESOURCE_REGISTRY")
                    .map(std::path::PathBuf::from)
                    .filter(|path| path.is_absolute() && path.is_dir())
                    .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
                    .ok_or_else(|| {
                        RunnerError::new("Desktop E2E requires its isolated machine registry")
                    })?,
            )
        } else {
            None
        };
        #[cfg(feature = "desktop-e2e")]
        let executable = fixture
            .as_ref()
            .map_or(executable, std::path::PathBuf::from);
        if !executable.is_absolute() || !executable.is_file() {
            return Err(RunnerError::new("The adjacent moyai-runner.exe is missing"));
        }
        let paths = crate::storage::StoragePaths::discover().map_err(error)?;
        std::fs::create_dir_all(&paths.data_dir).map_err(error)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.data_dir.join("runner.log"))
            .map_err(error)?;
        let mut command = std::process::Command::new(executable);
        #[cfg(feature = "desktop-e2e")]
        if let Some(registry) = fixture_registry {
            command
                .args([
                    "--exact",
                    "runner::shared::process_fixture::isolated_runner_process",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("MOYAI_TEST_RESOURCE_REGISTRY", registry)
                .env_remove("MOYAI_TEST_SHARED_SETTINGS");
        } else {
            command.args(["serve", "--background"]);
        }
        #[cfg(not(feature = "desktop-e2e"))]
        command.args(["serve", "--background"]);
        command
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().map_err(error)?)
            .stderr(log)
            .spawn()
            .map_err(error)
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new("Runner launch supports Windows only"))
    }
}

/// Read-only readiness after at most one launch. Accepted work is never replayed here.
pub(crate) fn ensure_started() -> Result<super::RunnerIdentity, RunnerError> {
    #[cfg(windows)]
    {
        let mut child = if !super::windows::endpoint_present()? {
            Some(launch_process()?)
        } else {
            None
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        loop {
            if super::windows::endpoint_present()? {
                return match super::windows::request(&super::RunnerCommand::Identity)? {
                    super::RunnerResponse::Identity { identity } => Ok(identity),
                    _ => Err(RunnerError::new(
                        "Unexpected execution host identity response",
                    )),
                };
            }
            if let Some(child) = &mut child {
                if let Some(status) = child.try_wait().map_err(error)? {
                    return Err(RunnerError::new(format!(
                        "Execution host could not start ({status}); see runner.log"
                    )));
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(RunnerError::new(
                    "Execution host startup is still unconfirmed; see runner.log",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new("Desktop execution supports Windows only"))
    }
}
