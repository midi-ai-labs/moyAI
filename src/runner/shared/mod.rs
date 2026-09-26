//! Hub-managed execution with a local durable start journal and explicit environment authority.
mod data;
pub mod external;
mod journal;
#[cfg(all(test, windows))]
mod process_fixture;
mod protocol;
pub(crate) mod provisioning;
mod settings;
pub(super) mod transport;

pub use protocol::{
    Assignment, AttemptStatus, Job, JobState, Report, ReportOutcome, SharedCandidate, SharedInput,
};
pub use settings::{EnvironmentMapping, ResourceScope, SharedSettings};

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ExecutionOutcome, LocalRunRequest, RunnerError, RunnerHost, SharedExecution};
use journal::{Entry, Journal, Phase};
use transport::{SharedClient, TransportError};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SharedProjection {
    pub connected: bool,
    pub accepting: bool,
    pub attempts: Vec<SharedAttemptProjection>,
    #[serde(default)]
    pub retained_services: Vec<RetainedServiceProjection>,
    pub error: Option<String>,
}

/// Local process evidence only. Job titles and private input are obtained separately
/// through the Hub's authorized project view.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetainedServiceProjection {
    pub service_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub project_id: String,
    pub conversation_id: String,
    pub environment_id: String,
    pub expires_at_ms: u64,
    pub local_state: String,
    pub uncertain: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedAttemptProjection {
    pub attempt_id: String,
    pub generation: u64,
    pub job_id: String,
    /// Opaque key for matching a separately authorized Hub project view.
    #[serde(default)]
    pub project_id: String,
    pub environment_id: String,
    pub run_id: ulid::Ulid,
    pub state: String,
    /// Live state of the exact local run. A journal receipt alone is not proof that
    /// its worker or managed processes have stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_state: Option<super::LocalRunState>,
}

/// Kept by the process entrypoint until its final local outcomes have reached the journal.
pub struct SharedWorker {
    task: tokio::task::JoinHandle<()>,
}

impl SharedWorker {
    pub async fn start(
        host: RunnerHost,
        mut settings: SharedSettings,
    ) -> Result<Self, RunnerError> {
        let locally_selected = host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .provisions
            .iter()
            .filter(|receipt| receipt.local_folder_environment())
            .map(|receipt| receipt.environment_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        // A deleted or replaced user-selected folder becomes unbound. It cannot
        // acquire a different path merely because the same saved string resolves.
        settings.environments.retain(|mapping| {
            !locally_selected.contains(&mapping.environment_id)
                || std::fs::canonicalize(&mapping.directory)
                    .ok()
                    .and_then(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
                    .as_ref()
                    == Some(&mapping.directory)
        });
        settings.resolve()?;
        let client = SharedClient::load(&settings, &host)?;
        let path = Journal::path_for(&host.inner.process.store().paths().data_dir, &settings)?;
        let journal = Journal::open(&path, &settings)?;
        crate::runtime::resource_admission::register(&settings)?;
        let (sender, commands) = tokio::sync::mpsc::channel(16);
        let (external_sender, external) = tokio::sync::mpsc::channel(16);
        {
            let mut state = host
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            if state.closing || state.shared_mode || !state.runs.is_empty() {
                return Err(RunnerError::new(
                    "Shared mode must be selected before any Runner work is accepted",
                ));
            }
            state.shared_mode = true;
            state.shared_projection = Some(SharedProjection::default());
            state.shared_commands = Some(sender);
            state.external_commands = Some(external_sender);
        }
        {
            let mut store = host
                .inner
                .operations
                .lock()
                .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
            let mut next = store.installed.clone();
            next.settings = Some(settings.clone());
            store.update(next)?;
        }
        let task = tokio::spawn(
            Controller {
                host,
                settings,
                client,
                journal,
                checkpoint_cursor: String::new(),
                commands: Some(commands),
                external: Some(external),
                local_project_id: None,
            }
            .run(),
        );
        Ok(Self { task })
    }

    pub async fn wait(self) -> Result<(), RunnerError> {
        self.task
            .await
            .map_err(|_| RunnerError::new("Shared Runner controller stopped unexpectedly"))
    }
}

struct Controller {
    host: RunnerHost,
    settings: SharedSettings,
    client: SharedClient,
    journal: Journal,
    checkpoint_cursor: String,
    commands: Option<tokio::sync::mpsc::Receiver<OperatorRequest>>,
    external: Option<tokio::sync::mpsc::Receiver<external::ExternalRequest>>,
    local_project_id: Option<String>,
}

pub(crate) struct OperatorRequest {
    pub operation: super::operations::RunnerOperation,
    pub reply: tokio::sync::oneshot::Sender<Result<(), RunnerError>>,
}

impl Controller {
    async fn process_operations(&mut self) {
        let request = self
            .commands
            .as_mut()
            .and_then(|queue| queue.try_recv().ok());
        if let Some(request) = request {
            let result = self.operator_request(request.operation).await;
            let _ = request.reply.send(result);
        }
    }

    async fn operator_request(
        &mut self,
        operation: super::operations::RunnerOperation,
    ) -> Result<(), RunnerError> {
        use super::operations::{ReconciliationEvidence, RunnerOperation};
        match operation {
            RunnerOperation::QuiescentShutdown {
                expected_desktop_binding,
            } => {
                // This queue runs between complete ticks: a claim in flight must
                // settle into the journal before the following check can pass.
                let store = self
                    .host
                    .inner
                    .operations
                    .lock()
                    .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
                if store.installed.desktop_binding.as_deref() != Some(&expected_desktop_binding)
                    || matches!(
                        store.installed.mode,
                        super::operations::ProvisionMode::Available
                    )
                    || store.installed.maintenance_until_ms.is_some()
                    || !self.journal.active()?.is_empty()
                {
                    return Err(RunnerError::new(
                        "Pause this Runner and wait for all work and unknown attempts before changing the Hub endpoint",
                    ));
                }
                self.host.begin_quiescent_shutdown()
            }
            RunnerOperation::LocalProject { project_id } => {
                if let Some(project_id) = &project_id {
                    if !crate::device_network::stable_id(project_id) {
                        return Err(RunnerError::new("A valid Hub project ID is required"));
                    }
                    let principal = self.device_principal(project_id).await?;
                    let projects: Vec<crate::device_network::WorkProject> = self
                        .client
                        .request_as(
                            "/v1/shared/projects",
                            None,
                            Some(&principal.principal_session),
                        )
                        .await?;
                    if !projects
                        .iter()
                        .any(|project| project.id == *project_id && project.allows_submission())
                    {
                        return Err(RunnerError::new(
                            "This device is not allowed to submit work in that project",
                        ));
                    }
                }
                self.local_project_id = project_id;
                Ok(())
            }
            RunnerOperation::Provision {
                template_id,
                environment_id,
            } => {
                self.provision_environment(&template_id, &environment_id)
                    .await
            }
            RunnerOperation::BindProjectFolder {
                project_id,
                environment_id,
                directory,
                access_mode,
                expected_directory,
            } => {
                self.bind_project_folder(
                    &project_id,
                    &environment_id,
                    directory,
                    access_mode,
                    expected_directory,
                )
                .await
            }
            RunnerOperation::ReconcileUnknown {
                attempt_id,
                generation,
                reason,
                evidence,
            } => {
                if reason.trim().is_empty() || reason.len() > 4096 {
                    return Err(RunnerError::new(
                        "A reconciliation reason of 1 to 4096 bytes is required",
                    ));
                }
                let mut entry = self
                    .journal
                    .get(&attempt_id)?
                    .ok_or_else(|| RunnerError::new("Unknown attempt is missing"))?;
                if entry.assignment.generation != generation || entry.phase != Phase::Uncertain {
                    return Err(RunnerError::new("This exact attempt is no longer unknown"));
                }
                if let Some(external) = &entry.external {
                    if crate::runtime::resource_admission::request_is_live(external.request_id)? {
                        return Err(RunnerError::new(
                            "The local participant still owns this exact execution. Stop it and wait for its processes before confirming recovery.",
                        ));
                    }
                }
                {
                    let mut state = self
                        .host
                        .inner
                        .state
                        .lock()
                        .map_err(|_| RunnerError::new("Runner unavailable"))?;
                    if let Some(run) = state.runs.get_mut(&entry.run_id) {
                        run.process_lifetime.cancel();
                        run.observe_process_completion(&self.host.inner.process.managed_shells());
                        if !run.processes_drained {
                            return Err(RunnerError::new(
                                "The current Runner still owns live work; Stop and wait for its process drain",
                            ));
                        }
                    } else if matches!(evidence, ReconciliationEvidence::ProcessDrain) {
                        return Err(RunnerError::new(
                            "No exact process-drain receipt survives here; local operator verification is required",
                        ));
                    }
                }
                if matches!(
                    evidence,
                    ReconciliationEvidence::OperatorConfirmedStopped {
                        effects_reviewed: false,
                        ..
                    } | ReconciliationEvidence::OperatorConfirmedStopped {
                        processes_stopped: false,
                        ..
                    }
                ) {
                    return Err(RunnerError::new(
                        "Operator reconciliation requires explicit effect review and stopped-process confirmation",
                    ));
                }
                let report = Report::for_assignment(
                    &entry.assignment,
                    "operator_reconciled",
                    ReportOutcome::Finished {
                        success: false,
                        resources_released: true,
                        result: json!({"version":1,"error":"Unknown execution was reconciled without replay", "reconciliation":{"evidence":evidence,"reason":reason,"at_ms":super::operations::now_ms(),"runner_id":self.host.identity().runner_id,"operator_sid":crate::runtime::resource_admission::operator_sid()?}}),
                    },
                );
                self.journal.reconciled(&mut entry, report)?;
                self.flush_report(&mut entry).await
            }
            RunnerOperation::StopShared {
                attempt_id,
                generation,
                run_id,
            } => {
                let entry = self
                    .journal
                    .get(&attempt_id)?
                    .ok_or_else(|| RunnerError::new("This received work is no longer active"))?;
                if entry.assignment.generation != generation
                    || entry.run_id != run_id
                    || entry.phase != Phase::Executing
                    || entry.external.is_some()
                {
                    return Err(RunnerError::new(
                        "This received work changed; refresh its status before stopping it",
                    ));
                }
                {
                    let state = self
                        .host
                        .inner
                        .state
                        .lock()
                        .map_err(|_| RunnerError::new("Runner unavailable"))?;
                    if !state.runs.get(&run_id).is_some_and(|run| run.shared) {
                        return Err(RunnerError::new(
                            "This exact received execution is no longer owned here",
                        ));
                    }
                }
                self.host
                    .on_executor(move |host| async move { host.stop_execution(run_id).await })
                    .await
            }
            RunnerOperation::StopRetainedService {
                service_id,
                attempt_id,
                generation,
            } => {
                let entry = self.journal.get(&attempt_id)?.ok_or_else(|| {
                    RunnerError::new("This received service is no longer recorded")
                })?;
                let service = entry
                    .retained_service
                    .ok_or_else(|| RunnerError::new("This attempt has no retained service"))?;
                if entry.assignment.generation != generation
                    || service.service_id.to_string() != service_id
                    || entry.service_stopped_ack
                {
                    return Err(RunnerError::new(
                        "This received service changed; refresh its status before stopping it",
                    ));
                }
                if !self
                    .host
                    .inner
                    .process
                    .managed_shells()
                    .cancel_retained_service(service.service_id)
                {
                    return Err(RunnerError::new(
                        "The exact local process handle is unavailable; confirm cleanup before reconciliation",
                    ));
                }
                Ok(())
            }
            RunnerOperation::ReconcileRetainedService {
                service_id,
                attempt_id,
                generation,
                reason,
                evidence,
            } => {
                if reason.trim().is_empty() || reason.len() > 4096 {
                    return Err(RunnerError::new(
                        "A reconciliation reason of 1 to 4096 bytes is required",
                    ));
                }
                let mut entry = self
                    .journal
                    .get(&attempt_id)?
                    .ok_or_else(|| RunnerError::new("This retained service is not recorded"))?;
                let service = entry
                    .retained_service
                    .ok_or_else(|| RunnerError::new("This attempt has no retained service"))?;
                if entry.assignment.generation != generation
                    || service.service_id.to_string() != service_id
                    || entry.service_stopped_ack
                {
                    return Err(RunnerError::new(
                        "This exact retained service changed; refresh before reconciliation",
                    ));
                }
                let shells = self.host.inner.process.managed_shells();
                let local_state = shells.retained_service_state(service);
                if matches!(
                    local_state,
                    crate::tool::shell::RetainedServiceState::Running
                        | crate::tool::shell::RetainedServiceState::Stopping
                ) {
                    return Err(RunnerError::new(
                        "The exact managed process may still be running; stop and wait",
                    ));
                }
                match &evidence {
                    ReconciliationEvidence::ProcessDrain
                        if local_state == crate::tool::shell::RetainedServiceState::Stopped =>
                    {
                        let mut state = self
                            .host
                            .inner
                            .state
                            .lock()
                            .map_err(|_| RunnerError::new("Runner unavailable"))?;
                        let run = state.runs.get_mut(&entry.run_id).ok_or_else(|| {
                            RunnerError::new("No exact process-drain receipt survives here")
                        })?;
                        run.process_lifetime.cancel();
                        run.observe_process_completion(&shells);
                        if !run.processes_drained {
                            return Err(RunnerError::new(
                                "The exact managed process is still draining",
                            ));
                        }
                    }
                    ReconciliationEvidence::OperatorConfirmedStopped {
                        effects_reviewed: true,
                        processes_stopped: true,
                    } if local_state == crate::tool::shell::RetainedServiceState::Unknown => {}
                    _ => {
                        return Err(RunnerError::new(
                            "Reconciliation requires exact process drain or explicit operator confirmation of effects and cleanup",
                        ));
                    }
                }
                self.journal.reconcile_service_stopped(
                    &mut entry,
                    json!({"reason":reason,"evidence":evidence,"at_ms":super::operations::now_ms(),
                        "operator_sid":crate::runtime::resource_admission::operator_sid()?}),
                )
            }
            _ => Err(RunnerError::new("Invalid shared operator command")),
        }
    }

    async fn run(mut self) {
        let recovery = self.recover();
        if let Err(error) = recovery {
            self.project(false, Some(error));
            self.host.wait_stopped().await;
            return;
        }
        loop {
            self.process_external().await;
            self.process_operations().await;
            let result = self.tick().await;
            let connected = result.is_ok();
            if !connected {
                // A retained server may not continue serving after the Hub authority
                // becomes unreachable. Keep the lease occupied until drain is proven.
                self.cancel_retained_services();
            }
            // Delivery of old checkpoint cleanup is not admission for current work. It has
            // no execution effects and runs after current stop/approval/claim processing.
            let cleanup = if connected && !self.closing() {
                self.reconcile_checkpoints().await
            } else {
                Ok(())
            };
            self.project(connected, result.err().or(cleanup.err()));
            if self.host.is_stopped() {
                // tick persisted every drained outcome before any network call. A missing
                // report acknowledgement is safe to retry from the journal on the next start.
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.client.shutdown().await;
    }

    fn closing(&self) -> bool {
        self.host
            .inner
            .state
            .lock()
            .map_or(true, |state| state.closing)
    }

    fn cancel_retained_services(&self) {
        if let Ok(entries) = self.journal.retained_services() {
            let shells = self.host.inner.process.managed_shells();
            for entry in entries {
                if let Some(service) = entry.retained_service {
                    shells.cancel_retained_service(service.service_id);
                }
            }
        }
    }

    async fn stop_fenced_executions(
        &mut self,
        assignments: Option<&[Assignment]>,
    ) -> Result<(), RunnerError> {
        for entry in self.journal.active()? {
            if entry.phase != Phase::Executing || entry.external.is_some() {
                continue;
            }
            let delivered = assignments.and_then(|assignments| {
                assignments
                    .iter()
                    .find(|assignment| assignment.attempt_id == entry.assignment.attempt_id)
            });
            let changed = delivered.is_some_and(|assignment| {
                assignment.generation != entry.assignment.generation
                    || assignment.runner_id != entry.assignment.runner_id
                    || assignment.job.id != entry.assignment.job.id
            });
            if delivered.is_none_or(|assignment| assignment.stop_requested) || changed {
                let id = entry.run_id;
                self.host
                    .on_executor(move |host| async move { host.stop_execution(id).await })
                    .await?;
            }
            if changed {
                return Err(RunnerError::new("Hub attempt identity changed"));
            }
        }
        Ok(())
    }

    async fn reconcile_retained_services(&mut self) -> Result<(), RunnerError> {
        use crate::tool::shell::RetainedServiceState;

        let retained = self.journal.retained_services()?;
        if retained.is_empty() {
            return Ok(());
        }
        let leases = self.client.services().await?;
        if leases.len() > 128 {
            return Err(RunnerError::new(
                "Hub retained-service count exceeds the Runner bound",
            ));
        }
        let shells = self.host.inner.process.managed_shells();
        for mut entry in retained {
            if !entry.retention_reported {
                continue;
            }
            let service = entry.retained_service.expect("retained-service query");
            let id = service.service_id.to_string();
            let lease = leases.iter().find(|lease| lease.service_id == id);
            if let Some(lease) = lease {
                if lease.attempt_id != entry.assignment.attempt_id
                    || lease.generation != entry.assignment.generation
                    || lease.conversation_id != entry.assignment.job.conversation_id
                    || lease.environment_id != entry.assignment.job.environment_id
                    || lease.expires_at_ms != service.expires_at_ms
                {
                    shells.cancel_retained_service(service.service_id);
                    return Err(RunnerError::new("Hub retained-service identity changed"));
                }
            }
            if entry.service_reconciliation.is_some() {
                let report = Report::for_assignment(
                    &entry.assignment,
                    &format!("service_stopped:{id}"),
                    ReportOutcome::ServiceStopped { service_id: id },
                );
                self.client.report(&report).await?;
                self.journal.service_stopped(&mut entry)?;
                continue;
            }
            let expired = service.expires_at_ms <= super::operations::now_ms();
            if lease.is_none() || lease.is_some_and(|lease| lease.stop_requested) || expired {
                shells.cancel_retained_service(service.service_id);
            }
            match shells.retained_service_state(service) {
                RetainedServiceState::Stopped => {
                    let drained = {
                        let mut state = self
                            .host
                            .inner
                            .state
                            .lock()
                            .map_err(|_| RunnerError::new("Runner unavailable"))?;
                        let Some(run) = state.runs.get_mut(&entry.run_id) else {
                            return Err(RunnerError::new(
                                "Retained service lost its exact local run receipt",
                            ));
                        };
                        run.process_lifetime.cancel();
                        run.observe_process_completion(&shells);
                        run.processes_drained
                    };
                    if !drained {
                        continue;
                    }
                    let report = Report::for_assignment(
                        &entry.assignment,
                        &format!("service_stopped:{id}"),
                        ReportOutcome::ServiceStopped { service_id: id },
                    );
                    self.client.report(&report).await?;
                    self.journal.service_stopped(&mut entry)?;
                }
                RetainedServiceState::Unknown if !entry.service_uncertain_ack => {
                    let report = Report::for_assignment(
                        &entry.assignment,
                        &format!("service_uncertain:{id}"),
                        ReportOutcome::ServiceUncertain {
                            service_id: id,
                            reason: "Runner restarted or lost the exact process handle; local cleanup requires confirmation".into(),
                        },
                    );
                    self.client.report(&report).await?;
                    self.journal.service_uncertain(&mut entry)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn recover(&mut self) -> Result<(), RunnerError> {
        for mut entry in self.journal.active()? {
            if entry.phase == Phase::Executing {
                if let Some(external) = &entry.external {
                    if !crate::runtime::resource_admission::request_is_live(external.request_id)? {
                        self.journal.uncertain(&mut entry, "The local participant stopped without a final resource receipt; review effects and process cleanup before reconciliation")?;
                    }
                    continue;
                }
                self.journal.uncertain(&mut entry, "Runner restarted after execution could have begun. Effects and process cleanup require reconciliation; automatic reexecution is disabled.")?;
            }
        }
        Ok(())
    }

    fn project(&self, connected: bool, error: Option<RunnerError>) {
        let accepting = self.host.accepting_new_shared() && !self.settings.environments.is_empty();
        let entries = self.journal.active();
        let (attempts, journal_error) = match entries {
            Ok(entries) => (
                entries
                    .into_iter()
                    .map(|entry| SharedAttemptProjection {
                        local_state: if entry.phase == Phase::Executing && entry.external.is_none()
                        {
                            Some(
                                self.host
                                    .snapshot(entry.run_id)
                                    .map_or(super::LocalRunState::Unknown, |run| run.state),
                            )
                        } else {
                            None
                        },
                        attempt_id: entry.assignment.attempt_id,
                        generation: entry.assignment.generation,
                        job_id: entry.assignment.job.id,
                        project_id: entry.assignment.job.project_id,
                        environment_id: entry.assignment.job.environment_id,
                        run_id: entry.run_id,
                        state: match entry.phase {
                            Phase::Intent => "preparing",
                            Phase::Executing => "executing",
                            Phase::ReportPending => "report_pending",
                            Phase::Uncertain => "unknown",
                            Phase::Settled => "settled",
                        }
                        .into(),
                    })
                    .collect::<Vec<_>>(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error)),
        };
        let (retained_services, service_error) = match self.journal.retained_services() {
            Ok(entries) => (
                entries
                    .into_iter()
                    .filter_map(|entry| {
                        let service = entry.retained_service?;
                        let local_state = match self
                            .host
                            .inner
                            .process
                            .managed_shells()
                            .retained_service_state(service)
                        {
                            crate::tool::shell::RetainedServiceState::Running => "running",
                            crate::tool::shell::RetainedServiceState::Stopping => "stopping",
                            crate::tool::shell::RetainedServiceState::Stopped => "stopped",
                            crate::tool::shell::RetainedServiceState::Unknown => "unknown",
                        };
                        Some(RetainedServiceProjection {
                            service_id: service.service_id.to_string(),
                            attempt_id: entry.assignment.attempt_id,
                            generation: entry.assignment.generation,
                            project_id: entry.assignment.job.project_id,
                            conversation_id: entry.assignment.job.conversation_id,
                            environment_id: entry.assignment.job.environment_id,
                            expires_at_ms: service.expires_at_ms,
                            local_state: local_state.into(),
                            uncertain: entry.service_uncertain_ack || local_state == "unknown",
                        })
                    })
                    .collect(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error)),
        };
        if let Ok(mut state) = self.host.inner.state.lock() {
            state.shared_projection = Some(SharedProjection {
                connected,
                accepting: accepting
                    && connected
                    && !state.closing
                    && attempts.is_empty()
                    && retained_services.is_empty()
                    && journal_error.is_none()
                    && service_error.is_none(),
                attempts,
                retained_services,
                error: error
                    .or(journal_error)
                    .or(service_error)
                    .map(|error| error.message),
            });
        }
    }

    async fn tick(&mut self) -> Result<(), RunnerError> {
        let retired = self.client.connection_retired().unwrap_or(true);
        if retired {
            self.host.begin_shutdown()?;
        }
        self.host.refresh_maintenance()?;
        // Observe Hub stop fences before collecting a terminal result or publishing a
        // retained process. The Hub still serializes reports with the stop transaction,
        // but this ordering closes the avoidable local window where a stopped worker
        // could be recorded as a successful preview before the next assignment poll.
        let assignments = if retired || self.closing() {
            None
        } else {
            Some(self.client.assignments(&self.settings).await)
        };
        match assignments.as_ref() {
            Some(Ok(assignments)) => {
                if assignments.len() > 128 {
                    self.stop_fenced_executions(None).await?;
                    return Err(RunnerError::new(
                        "Hub assignment count exceeds the Runner reconciliation bound",
                    ));
                }
                for assignment in assignments {
                    if let Err(error) = self.validate_assignment(assignment) {
                        self.stop_fenced_executions(None).await?;
                        return Err(error);
                    }
                }
                self.stop_fenced_executions(Some(assignments)).await?;
            }
            Some(Err(_)) => {
                // If this Runner cannot observe the Hub's current fence, stop its
                // active workers. Keep the journal and capacity until exact drain and
                // acknowledgement; a reconnect must never replay their effects.
                self.stop_fenced_executions(None).await?;
            }
            None => {}
        }
        // Durable results are saved even while the Hub is disconnected or during shutdown.
        for mut entry in self.journal.active()? {
            if entry.phase == Phase::Executing {
                if let Some(external) = &entry.external {
                    if !crate::runtime::resource_admission::request_is_live(external.request_id)? {
                        self.journal.uncertain(&mut entry, "The local participant stopped without a final resource receipt; review effects and process cleanup before reconciliation")?;
                    }
                    continue;
                }
                self.expire_approval(&entry);
                self.collect_outcome(&mut entry)?;
            }
        }
        if retired {
            return Err(RunnerError::new(
                "Hub connection was reset on this PC. Previous work remains recorded locally and is not submitted to another Hub.",
            ));
        }
        let assignments = match assignments {
            Some(Ok(assignments)) => Some(assignments),
            Some(Err(error)) => return Err(error.into()),
            None => None,
        };
        // An already accepted retained lease may have been fenced after the
        // previous tick. Stop and reconcile it before reporting its turn terminal.
        self.reconcile_retained_services().await?;
        for mut entry in self.journal.active()? {
            if entry.phase == Phase::ReportPending {
                self.flush_report(&mut entry).await?;
            } else if entry.phase == Phase::Uncertain {
                self.client
                    .report(
                        entry.report.as_ref().ok_or_else(|| {
                            RunnerError::new("Uncertain journal report is missing")
                        })?,
                    )
                    .await
                    .map_err(RunnerError::from)?;
            }
        }
        if self.closing() {
            return Ok(());
        }
        let assignments = assignments.expect("open Runner polls assignments before reconciliation");
        for assignment in &assignments {
            self.validate_assignment(assignment)?;
            if let Some(entry) = self.journal.get(&assignment.attempt_id)? {
                if entry.assignment.generation != assignment.generation {
                    return Err(RunnerError::new("Hub attempt generation changed"));
                }
            } else if assignment.job.state != JobState::Assigned || assignment.stop_requested {
                // An active attempt missing from this journal might belong to an earlier host.
                // No new claim or effect is permitted merely because this process has no worker.
                let mapping = self
                    .settings
                    .mapping(&assignment.job.environment_id)?
                    .clone();
                let mut entry = self.journal.intent(assignment.clone(), mapping)?;
                self.journal.executing(&mut entry)?;
                self.journal.uncertain(&mut entry, "Hub retains an active attempt with no local start evidence. Automatic execution and release are disabled.")?;
                return Ok(());
            }
        }
        for mut entry in self.journal.active()? {
            if entry.phase == Phase::Executing {
                self.relay_approval(&mut entry).await?;
            }
        }
        let active = self.journal.active()?;
        if active
            .iter()
            .any(|entry| entry.external.is_none() && entry.phase != Phase::Intent)
        {
            return Ok(());
        }
        if !self.host.accepting_new_shared() {
            return Ok(());
        }
        self.sync_provisioning().await?;
        if self.settings.environments.is_empty() {
            return Ok(());
        }
        self.validate_resource_capacity().await?;
        let pending = active.into_iter().find(|entry| entry.external.is_none());
        let mut entry = match pending {
            Some(entry) => entry,
            None => {
                let Some(assignment) = self.client.claim(&self.settings).await? else {
                    return Ok(());
                };
                self.validate_assignment(&assignment)?;
                let mapping = self
                    .settings
                    .mapping(&assignment.job.environment_id)?
                    .clone();
                self.journal.intent(assignment, mapping)?
            }
        };
        self.start(&mut entry).await
    }

    async fn reconcile_checkpoints(&mut self) -> Result<(), RunnerError> {
        let mut entries = self
            .journal
            .unsettled_checkpoints(&self.checkpoint_cursor)?;
        if entries.is_empty() && !self.checkpoint_cursor.is_empty() {
            self.checkpoint_cursor.clear();
            entries = self.journal.unsettled_checkpoints("")?;
        }
        if let Some(last) = entries.last() {
            self.checkpoint_cursor = last.assignment.attempt_id.clone();
        }
        // One historical delivery per tick preserves capacity for current stop and approval
        // requests, including the Hub's shared-operation concurrency budget.
        for mut entry in entries {
            let status = self.client.attempt(&entry.assignment.attempt_id).await?;
            let assignment = &status.assignment;
            if assignment.attempt_id != entry.assignment.attempt_id
                || assignment.generation != entry.assignment.generation
                || assignment.runner_id != entry.assignment.runner_id
                || assignment.job.id != entry.assignment.job.id
                || assignment.job.project_id != entry.assignment.job.project_id
                || assignment.job.environment_id != entry.assignment.job.environment_id
            {
                return Err(RunnerError::new(
                    "Checkpoint settlement attempt identity changed",
                ));
            }
            let Some(checkpoint) = entry.checkpoint() else {
                return Err(RunnerError::new("Checkpoint settlement receipt is missing"));
            };
            let settlement = self
                .host
                .inner
                .process
                .store()
                .session_repo()
                .settle_shared_checkpoint_terminal(
                    checkpoint,
                    &serde_json::to_value(&assignment.job)
                        .map_err(|error| RunnerError::new(error.to_string()))?,
                )
                .map_err(|error| RunnerError::new(error.to_string()))?;
            match settlement {
                crate::agent::shared::SharedCheckpointSettlement::Applied
                | crate::agent::shared::SharedCheckpointSettlement::NoLongerPaused => {
                    self.journal.acknowledge_checkpoint_settlement(&mut entry)?
                }
                crate::agent::shared::SharedCheckpointSettlement::Pending => {}
            }
        }
        Ok(())
    }

    fn validate_assignment(&self, assignment: &Assignment) -> Result<(), RunnerError> {
        assignment.require_supported_capabilities()?;
        let ids = [
            &assignment.attempt_id,
            &assignment.runner_id,
            &assignment.job.id,
            &assignment.job.root_id,
            &assignment.job.project_id,
            &assignment.job.environment_id,
            &assignment.job.requestor_id,
            &assignment.job.assignee_id,
        ];
        let child_ids = assignment
            .allowed_child_environments
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let candidate_ids = assignment
            .allowed_child_candidates
            .iter()
            .map(|candidate| &candidate.environment_id)
            .collect::<std::collections::BTreeSet<_>>();
        if assignment.runner_id != self.settings.device_id
            || assignment.generation == 0
            || ids.iter().any(|id| !crate::device_network::stable_id(id))
            || assignment
                .job
                .continued_from
                .as_ref()
                .is_some_and(|id| !crate::device_network::stable_id(id) || id == &assignment.job.id)
            || child_ids.len() != assignment.allowed_child_environments.len()
            || child_ids.len() > 128
            || child_ids
                .iter()
                .any(|id| !crate::device_network::stable_id(id))
            || candidate_ids.len() != assignment.allowed_child_candidates.len()
            || candidate_ids.len() > 128
            || assignment.allowed_child_candidates.iter().any(|candidate| {
                !child_ids.contains(&candidate.environment_id)
                    || !crate::device_network::stable_id(&candidate.device_id)
                    || candidate.device_label.trim().is_empty()
                    || candidate.device_label.len() > 256
                    || candidate.environment_label.trim().is_empty()
                    || candidate.environment_label.len() > 256
                    || candidate.capabilities.len() > 32
                    || candidate
                        .capabilities
                        .iter()
                        .any(|capability| capability.trim().is_empty() || capability.len() > 128)
            })
            || assignment.job.title.len() > 256
            || assignment.retained_services.len() > 16
            || assignment.retained_services.iter().any(|service| {
                service.service_id.parse::<ulid::Ulid>().is_err()
                    || !crate::device_network::stable_id(&service.attempt_id)
                    || service.generation == 0
                    || service.conversation_id != assignment.job.conversation_id
                    || !crate::device_network::stable_id(&service.environment_id)
            })
        {
            return Err(RunnerError::new("Hub assignment identity is invalid"));
        }
        Ok(())
    }

    fn resource_for_assignment(
        &self,
        entry: &Entry,
    ) -> Result<
        (
            std::sync::Arc<crate::runtime::resource_admission::ResourceGuard>,
            Vec<ulid::Ulid>,
        ),
        RunnerError,
    > {
        let leases = &entry.assignment.retained_services;
        if leases.is_empty() {
            return Ok((
                std::sync::Arc::new(crate::runtime::resource_admission::ResourceGuard::acquire(
                    &self.settings.resource_scope,
                    &entry.mapping.directory,
                )?),
                Vec::new(),
            ));
        }
        let shells = self.host.inner.process.managed_shells();
        let state = self
            .host
            .inner
            .state
            .lock()
            .map_err(|_| RunnerError::new("Runner unavailable"))?;
        let mut resource = None;
        let mut runs = Vec::new();
        for lease in leases {
            let old = self
                .journal
                .get(&lease.attempt_id)?
                .ok_or_else(|| RunnerError::new("Hub service lease has no local receipt"))?;
            let service = old.retained_service.ok_or_else(|| {
                RunnerError::new("Hub service lease has no local process receipt")
            })?;
            if old.assignment.generation != lease.generation
                || old.assignment.job.project_id != entry.assignment.job.project_id
                || old.assignment.job.conversation_id != lease.conversation_id
                || old.assignment.job.environment_id != lease.environment_id
                || old.mapping.directory != entry.mapping.directory
                || service.service_id.to_string() != lease.service_id
                || service.expires_at_ms != lease.expires_at_ms
                || !old.retention_reported
                || old.service_stopped_ack
                || !shells.retained_service_live(service)
            {
                return Err(RunnerError::new(
                    "Hub service lease is not live in this exact workspace",
                ));
            }
            let run = state.runs.get(&old.run_id).ok_or_else(|| {
                RunnerError::new("Hub service lease lost its local execution receipt")
            })?;
            if !run.shared || run.active() || run.processes_drained {
                return Err(RunnerError::new("Hub service lease is not locally settled"));
            }
            let held = run.resource.clone().ok_or_else(|| {
                RunnerError::new("Hub service lease lost its local resource guard")
            })?;
            if resource
                .as_ref()
                .is_some_and(|first| !std::sync::Arc::ptr_eq(first, &held))
            {
                return Err(RunnerError::new(
                    "Hub leases do not share one local resource owner",
                ));
            }
            resource = Some(held);
            runs.push(old.run_id);
        }
        Ok((resource.expect("nonempty leases"), runs))
    }

    async fn relay_approval(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        let Some(pending) = self.host.inner.approvals.pending(entry.run_id) else {
            return Ok(());
        };
        let approval_id = pending.approval_id.to_string();
        if !entry.approval_report.as_ref().is_some_and(|report| matches!(&report.outcome, ReportOutcome::ApprovalRequested { approval_id: id, .. } if *id == approval_id)) {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RunnerError::new("System clock is unavailable"))?.as_millis() as u64;
            let report = Report::for_assignment(&entry.assignment, &format!("approval:{approval_id}"), ReportOutcome::ApprovalRequested {
                approval_id: approval_id.clone(), request: serde_json::to_value(&pending.request).map_err(|error| RunnerError::new(error.to_string()))?,
                expires_at_ms: now.saturating_add(15 * 60 * 1000),
            });
            self.journal.approval(entry, report)?;
        }
        let report = entry
            .approval_report
            .as_ref()
            .expect("saved approval report");
        self.client
            .report(report)
            .await
            .map_err(RunnerError::from)?;
        if let Some(reply) = self.client.consume_approval(entry, &approval_id).await? {
            use protocol::ApprovalConsumeResult;
            let (ApprovalConsumeResult::Answer {
                approval_id: reply_id,
                ..
            }
            | ApprovalConsumeResult::ReconfirmationRequired {
                approval_id: reply_id,
                ..
            }) = &reply;
            if *reply_id != approval_id {
                return Err(RunnerError::new("Hub approval reply names another request"));
            }
            // The broker checks exact IDs and cancellation for both transitions. A late
            // answer cannot authorize the replacement or a different suspended tool.
            match reply {
                ApprovalConsumeResult::Answer { decision, .. } => {
                    self.host
                        .inner
                        .approvals
                        .answer(entry.run_id, pending.approval_id, decision);
                }
                ApprovalConsumeResult::ReconfirmationRequired {
                    reconfirmation_required: true,
                    ..
                } => {
                    self.host
                        .inner
                        .approvals
                        .reconfirm(entry.run_id, pending.approval_id);
                }
                ApprovalConsumeResult::ReconfirmationRequired {
                    reconfirmation_required: false,
                    ..
                } => {
                    return Err(RunnerError::new(
                        "Hub returned an invalid reconfirmation response",
                    ));
                }
            }
        }
        Ok(())
    }

    fn expire_approval(&self, entry: &Entry) {
        let Some(Report {
            outcome:
                ReportOutcome::ApprovalRequested {
                    approval_id,
                    expires_at_ms,
                    ..
                },
            ..
        }) = &entry.approval_report
        else {
            return;
        };
        let expired = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(true, |now| now.as_millis() >= u128::from(*expires_at_ms));
        if expired {
            if let Ok(id) = approval_id.parse() {
                self.host.inner.approvals.answer(
                    entry.run_id,
                    id,
                    super::LocalApprovalDecision::Stop,
                );
            }
        }
    }

    async fn start(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        let current_mapping = self
            .settings
            .mapping(&entry.assignment.job.environment_id)?;
        if *current_mapping != entry.mapping {
            return self
                .preparation_failed(
                    entry,
                    "The local environment authority changed before execution",
                )
                .await;
        }
        if std::fs::canonicalize(&entry.mapping.directory)
            .ok()
            .and_then(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
            .as_ref()
            != Some(&entry.mapping.directory)
        {
            return self
                .preparation_failed(entry, "The selected project folder is missing or changed")
                .await;
        }
        let input = serde_json::from_value::<SharedInput>(entry.assignment.job.input.clone())
            .map_err(|_| RunnerError::new("Unsupported shared input"))
            .and_then(|input| {
                input.validate()?;
                Ok(input)
            });
        let input = match input {
            Ok(input) => input,
            Err(error) => return self.preparation_failed(entry, &error.message).await,
        };
        let report = Report::for_assignment(&entry.assignment, "started", ReportOutcome::Started);
        match self.client.report(&report).await {
            Ok(_) => {}
            Err(TransportError::Rejected(_)) => {
                return self
                    .preparation_failed(entry, "Hub did not authorize execution")
                    .await;
            }
            Err(error) => return Err(error.into()),
        }
        let status = self.client.attempt(&entry.assignment.attempt_id).await?;
        if let Err(error) = self.validate_assignment(&status.assignment) {
            return self.preparation_failed(entry, &error.message).await;
        }
        if status.assignment.attempt_id != entry.assignment.attempt_id
            || status.assignment.generation != entry.assignment.generation
            || status.assignment.runner_id != self.settings.device_id
            || status.state != "running"
            || status.assignment.stop_requested
            || status.uncertainty_reason.is_some()
            || status.assignment.job.state != JobState::Running
            || self.closing()
        {
            return self
                .preparation_failed(entry, "Execution authorization changed before startup")
                .await;
        }
        let allowed_child_environments = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Execution settings are unavailable"))?
            .installed
            .child_environments(
                &entry.mapping,
                &status.assignment.allowed_child_environments,
            );
        let allowed_child_candidates = status
            .assignment
            .allowed_child_candidates
            .iter()
            .filter(|candidate| allowed_child_environments.contains(&candidate.environment_id))
            .cloned()
            .collect();
        self.journal.executing(entry)?;
        if let Err(error) = self.client.authorize(&status.assignment, None).await {
            return self.preparation_failed(entry, &error.message).await;
        }
        let (resource, compatible_retained_runs) = match self.resource_for_assignment(entry) {
            Ok(resource) => resource,
            Err(error) => return self.preparation_failed(entry, &error.message).await,
        };
        let prepared = match data::prepare(&self.client, entry, &input).await {
            Ok(prepared) => prepared,
            Err(error) => return self.preparation_failed(entry, &error.message).await,
        };
        let resume = match (
            &entry.assignment.job.checkpoint,
            &entry.assignment.child_result,
        ) {
            (None, None) => None,
            (Some(checkpoint), Some(child))
                if entry.assignment.job.awaiting_child_id.as_deref() == Some(child.id.as_str()) =>
            {
                Some(crate::agent::shared::SharedResume {
                    checkpoint: checkpoint.clone(),
                    archive: prepared.archive,
                    child_result: serde_json::to_value(child)
                        .map_err(|error| RunnerError::new(error.to_string()))?,
                })
            }
            _ => {
                return self
                    .preparation_failed(
                        entry,
                        "Checkpoint and child result do not form a resumable pair",
                    )
                    .await;
            }
        };
        let context = crate::agent::shared::SharedRunContext {
            job_id: entry.assignment.job.id.clone(),
            attempt_id: entry.assignment.attempt_id.clone(),
            generation: entry.assignment.generation,
            project_id: entry.assignment.job.project_id.clone(),
            environment_id: entry.assignment.job.environment_id.clone(),
            allowed_child_environments,
            allowed_child_candidates,
            resume,
            continuation: prepared.continuation,
        };
        let request = LocalRunRequest {
            directory: entry.mapping.directory.clone(),
            prompt: prepared.prompt,
            session_id: None,
            title: Some(entry.assignment.job.title.clone()),
            single_agent: true,
        };
        if let Err(error) = self
            .host
            .submit_execution(
                entry.run_id,
                request,
                Some(SharedExecution {
                    context,
                    model_client: self.client.clone(),
                    access_mode: entry.mapping.access_mode,
                    authority: self.client.effect_authority(entry.assignment.clone()),
                    resource,
                    compatible_retained_runs,
                }),
            )
            .await
        {
            let definitely_unaccepted = self
                .host
                .inner
                .state
                .lock()
                .is_ok_and(|state| !state.runs.contains_key(&entry.run_id));
            if !definitely_unaccepted {
                self.journal.uncertain(entry, "Local startup returned no usable receipt after acceptance could have occurred; process reconciliation is required.")?;
                return Err(error);
            }
            let report = Report::for_assignment(
                &entry.assignment,
                "finished",
                ReportOutcome::Finished {
                    success: false,
                    result: json!({"version":1,"error":error.message}),
                    resources_released: true,
                },
            );
            self.journal.outcome(entry, report)?;
        }
        Ok(())
    }

    async fn preparation_failed(
        &mut self,
        entry: &mut Entry,
        message: &str,
    ) -> Result<(), RunnerError> {
        let report = Report::for_assignment(
            &entry.assignment,
            "finished",
            ReportOutcome::Finished {
                success: false,
                result: json!({"version":1,"error":message}),
                resources_released: true,
            },
        );
        self.journal.outcome(entry, report)?;
        self.flush_report(entry).await
    }

    fn collect_outcome(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        let snapshot = self.host.snapshot(entry.run_id)?;
        let (outcome, cancelled, retained_service) = {
            let mut state = self
                .host
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            let run = state
                .runs
                .get_mut(&entry.run_id)
                .ok_or_else(|| RunnerError::new("Accepted shared execution is missing"))?;
            let proposed_service = match &run.result {
                Some(Ok(ExecutionOutcome::Yielded(yielded))) if !run.control.is_cancelled() => {
                    yielded.retained_service
                }
                Some(Ok(ExecutionOutcome::Completed(summary)))
                    if !run.control.is_cancelled()
                        && summary.status() == crate::session::SessionStatus::Completed =>
                {
                    self.host
                        .inner
                        .process
                        .managed_shells()
                        .completion_service_in_scope(summary.session_id(), run.managed_scope_id)
                }
                _ => None,
            };
            let retained_service = proposed_service.filter(|service| {
                self.host
                    .inner
                    .process
                    .managed_shells()
                    .retained_service_live(*service)
            });
            if run.result.is_some() && retained_service.is_none() {
                run.process_lifetime.cancel();
            }
            run.observe_process_completion(&self.host.inner.process.managed_shells());
            if run.active() || (retained_service.is_none() && !run.processes_drained) {
                return Ok(());
            }
            (
                run.result.clone(),
                run.control.is_cancelled(),
                retained_service,
            )
        };
        let local_checkpoint = match &outcome {
            Some(Ok(ExecutionOutcome::Yielded(yielded))) => Some(yielded.checkpoint.clone()),
            _ => None,
        };
        let outcome = match outcome {
            None => {
                self.journal.uncertain(entry, "The execution worker ended without a durable outcome. Automatic reexecution and release are disabled.")?;
                return Ok(());
            }
            Some(Ok(ExecutionOutcome::Yielded(_))) if cancelled => ReportOutcome::Finished {
                success: false,
                result: json!({"version":1,"error":"Execution was stopped before the child handoff"}),
                resources_released: true,
            },
            Some(Ok(ExecutionOutcome::Yielded(yielded)))
                if yielded.retained_service.is_some() && retained_service.is_none() =>
            {
                ReportOutcome::Finished {
                    success: false,
                    result: json!({"version":1,"error":"The retained service stopped before the child handoff"}),
                    resources_released: true,
                }
            }
            Some(Ok(ExecutionOutcome::Yielded(yielded))) => ReportOutcome::YieldToChild {
                checkpoint: yielded.checkpoint,
                child_environment_id: yielded.child.environment_id,
                child_title: yielded.child.title,
                child_input: yielded.child.input,
                resources_released: true,
            },
            Some(Ok(ExecutionOutcome::Completed(summary))) => ReportOutcome::Finished {
                success: !cancelled && summary.status() == crate::session::SessionStatus::Completed,
                result: json!({"version":1,"summary":summary,"text":snapshot.result_text,"truncated":snapshot.result_truncated,
                    "stopped_before_acknowledgement":cancelled,
                    "retained_service":retained_service.map(|service| json!({"service_id":service.service_id.to_string(),
                        "expires_at_ms":service.expires_at_ms}))}),
                resources_released: true,
            },
            Some(Err(error)) => ReportOutcome::Finished {
                success: false,
                result: json!({"version":1,"error":error}),
                resources_released: true,
            },
        };
        let report = Report::for_assignment(&entry.assignment, "outcome", outcome);
        self.journal
            .outcome_with_retention(entry, report, local_checkpoint, retained_service)
    }

    async fn flush_report(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        if let Some(service) = entry.retained_service {
            let shells = self.host.inner.process.managed_shells();
            if !entry.retention_reported
                && !entry.service_stopped_ack
                && entry.report.as_ref().is_some_and(|report| {
                    matches!(
                        report.outcome,
                        ReportOutcome::YieldToChild { .. }
                            | ReportOutcome::Finished { success: true, .. }
                    )
                })
            {
                let id = service.service_id.to_string();
                let retain = Report::for_assignment(
                    &entry.assignment,
                    &format!("retain_service:{id}"),
                    ReportOutcome::RetainService {
                        service_id: id,
                        expires_at_ms: service.expires_at_ms,
                    },
                );
                match self.client.report(&retain).await {
                    Ok(_) => self.journal.retention_reported(entry)?,
                    Err(TransportError::Rejected(_)) => {
                        shells.cancel_retained_service(service.service_id);
                        self.journal.fail_retained_handoff(
                            entry,
                            "Hub rejected the finite service lease before the turn completed",
                        )?;
                        return Ok(());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if entry.report.as_ref().is_some_and(|report| {
                matches!(
                    report.outcome,
                    ReportOutcome::YieldToChild { .. }
                        | ReportOutcome::Finished { success: true, .. }
                )
            }) && !shells.retained_service_live(service)
            {
                shells.cancel_retained_service(service.service_id);
                if data::report_frozen(&self.host, entry)? {
                    // The exact success/yield report may already be accepted despite a
                    // lost acknowledgement. Keep its payload and event ID immutable,
                    // and let the stopped service's separate lease receipt drain first.
                    if !entry.service_stopped_ack {
                        return Ok(());
                    }
                } else {
                    self.journal.fail_retained_handoff(
                        entry,
                        "The retained service stopped before the turn completed",
                    )?;
                    return Ok(());
                }
            }
            if entry.report.as_ref().is_some_and(|report| {
                matches!(
                    report.outcome,
                    ReportOutcome::Finished { success: false, .. }
                )
            }) && !entry.service_stopped_ack
            {
                // An accepted lease is reconciled with Hub before the root reports
                // completion. A rejected lease must still drain locally first.
                if entry.retention_reported {
                    return Ok(());
                }
                if entry.service_reconciliation.is_some() {
                    self.journal.service_stopped(entry)?;
                } else {
                    let drained = {
                        let mut state = self
                            .host
                            .inner
                            .state
                            .lock()
                            .map_err(|_| RunnerError::new("Runner unavailable"))?;
                        let run = state.runs.get_mut(&entry.run_id).ok_or_else(|| {
                            RunnerError::new("Retained service lost its exact local run receipt")
                        })?;
                        run.process_lifetime.cancel();
                        run.observe_process_completion(&shells);
                        run.processes_drained
                    };
                    if !drained {
                        return Ok(());
                    }
                    self.journal.service_stopped(entry)?;
                }
            }
        }
        if entry.external.is_none() {
            data::persist(&self.client, &self.host, entry).await?;
        }
        let report = entry
            .report
            .as_ref()
            .ok_or_else(|| RunnerError::new("Durable outcome report is missing"))?;
        match self.client.report(report).await {
            Ok(_) => {}
            Err(TransportError::Rejected(status))
                if matches!(report.outcome, ReportOutcome::YieldToChild { .. })
                    && matches!(status.as_u16(), 400 | 403 | 409 | 429) =>
            {
                let already_released = match self.client.attempt(&entry.assignment.attempt_id).await
                {
                    Ok(status) => {
                        if status.assignment.attempt_id != entry.assignment.attempt_id
                            || status.assignment.generation != entry.assignment.generation
                            || status.assignment.runner_id != self.settings.device_id
                        {
                            return Err(RunnerError::new(
                                "Child handoff reconciliation names a different attempt",
                            ));
                        }
                        matches!(
                            status.state.as_str(),
                            "yielded" | "succeeded" | "failed" | "cancelled"
                        )
                    }
                    // Revoked devices may report drained effects while their reads are denied.
                    // A conflicting earlier yield rejects the fallback; keep both records then.
                    Err(_) => false,
                };
                if !already_released {
                    if let Some(service) = entry.retained_service {
                        self.host
                            .inner
                            .process
                            .managed_shells()
                            .cancel_retained_service(service.service_id);
                    }
                    let fallback = Report::for_assignment(
                        &entry.assignment,
                        "yield_rejected",
                        ReportOutcome::Finished {
                            success: false,
                            result: json!({"version":1,"error":"Hub did not accept the child handoff"}),
                            resources_released: true,
                        },
                    );
                    // Keep the original: a previously timed-out report may still commit before
                    // this fallback. Replaying it first resolves that race without reexecution.
                    self.journal
                        .retain_rejected_yield_fallback(entry, fallback)?;
                    self.client
                        .report(entry.fallback_report.as_ref().expect("saved fallback"))
                        .await
                        .map_err(RunnerError::from)?;
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.journal.settled(entry)?;
        if entry.external.is_none() {
            data::settled(&self.host, entry)?;
        }
        // Hub's attempt/report journal owns shared idempotency. Only a fully acknowledged,
        // drained shared receipt can be retired; private receipt retention is unchanged.
        if let Ok(mut state) = self.host.inner.state.lock() {
            if state
                .runs
                .get(&entry.run_id)
                .is_some_and(|run| run.processes_drained)
            {
                state.runs.remove(&entry.run_id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod integration_fixture;

#[cfg(test)]
mod transport_tests;

#[cfg(test)]
mod integration_tests;
