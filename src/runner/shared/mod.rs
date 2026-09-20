//! Hub-managed execution with a local durable start journal and explicit environment authority.
mod data;
pub mod external;
mod journal;
#[cfg(all(test, windows))]
mod process_fixture;
mod protocol;
pub(crate) mod provisioning;
mod settings;
mod transport;

pub use protocol::{Assignment, AttemptStatus, Job, JobState, Report, ReportOutcome, SharedInput};
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
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedAttemptProjection {
    pub attempt_id: String,
    pub generation: u64,
    pub job_id: String,
    pub environment_id: String,
    pub run_id: ulid::Ulid,
    pub state: String,
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
        settings.resolve()?;
        let client = SharedClient::load(&settings, &host)?;
        let path = host
            .inner
            .process
            .store()
            .paths()
            .data_dir
            .join("runner-shared.sqlite3");
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
                local_human: None,
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
    local_human: Option<external::LocalHuman>,
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
            RunnerOperation::LocalSignIn { credentials } => {
                if !crate::device_network::stable_id(&credentials.project_id)
                    || credentials.username.trim().is_empty()
                    || credentials.username.len() > 256
                    || credentials.password.len() > 1024
                {
                    return Err(RunnerError::new(
                        "Valid Hub credentials and a project ID are required",
                    ));
                }
                #[derive(Deserialize)]
                struct Login {
                    token: String,
                }
                let login:Login=self.client.request("/v1/shared/login",Some(&json!({"username":credentials.username,"password":credentials.password}))).await?;
                self.local_human = Some(external::LocalHuman {
                    actor_device_id: self.settings.device_id.clone(),
                    principal_session: login.token,
                    project_id: credentials.project_id,
                });
                Ok(())
            }
            RunnerOperation::LocalSignOut => {
                if let Some(human) = self.local_human.take() {
                    let _: serde_json::Value = self
                        .client
                        .request_as(
                            "/v1/shared/logout",
                            Some(&json!({})),
                            Some(&human.principal_session),
                        )
                        .await?;
                }
                Ok(())
            }
            RunnerOperation::Provision {
                template_id,
                environment_id,
            } => {
                self.provision_environment(&template_id, &environment_id)
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
                        attempt_id: entry.assignment.attempt_id,
                        generation: entry.assignment.generation,
                        job_id: entry.assignment.job.id,
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
        if let Ok(mut state) = self.host.inner.state.lock() {
            state.shared_projection = Some(SharedProjection {
                connected,
                accepting: accepting
                    && connected
                    && !state.closing
                    && attempts.is_empty()
                    && journal_error.is_none(),
                attempts,
                error: error.or(journal_error).map(|error| error.message),
            });
        }
    }

    async fn tick(&mut self) -> Result<(), RunnerError> {
        self.host.refresh_maintenance()?;
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
        let assignments = self.client.assignments(&self.settings).await?;
        if assignments.len() > 128 {
            return Err(RunnerError::new(
                "Hub assignment count exceeds the Runner reconciliation bound",
            ));
        }
        for assignment in &assignments {
            self.validate_assignment(assignment)?;
            if let Some(entry) = self.journal.get(&assignment.attempt_id)? {
                if entry.assignment.generation != assignment.generation {
                    return Err(RunnerError::new("Hub attempt generation changed"));
                }
                if entry.phase == Phase::Executing && assignment.stop_requested {
                    if entry.external.is_some() {
                        continue;
                    }
                    let id = entry.run_id;
                    self.host
                        .on_executor(move |host| async move { host.stop_execution(id).await })
                        .await?;
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
        if assignment.runner_id != self.settings.device_id
            || assignment.generation == 0
            || ids.iter().any(|id| !crate::device_network::stable_id(id))
            || assignment.job.title.len() > 256
        {
            return Err(RunnerError::new("Hub assignment identity is invalid"));
        }
        Ok(())
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
        self.journal.executing(entry)?;
        if let Err(error) = self.client.authorize(&status.assignment, None).await {
            return self.preparation_failed(entry, &error.message).await;
        }
        let resource = match crate::runtime::resource_admission::ResourceGuard::acquire(
            &self.settings.resource_scope,
            &entry.mapping.directory,
        ) {
            Ok(resource) => std::sync::Arc::new(resource),
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
            project_id: entry.assignment.job.project_id.clone(),
            environment_id: entry.assignment.job.environment_id.clone(),
            allowed_child_environments,
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
                    access_mode: entry.mapping.access_mode,
                    authority: self.client.effect_authority(entry.assignment.clone()),
                    resource,
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
        let (outcome, cancelled) = {
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
            if run.result.is_some() {
                run.process_lifetime.cancel();
            }
            run.observe_process_completion(&self.host.inner.process.managed_shells());
            if run.active() || !run.processes_drained {
                return Ok(());
            }
            (run.result.clone(), run.control.is_cancelled())
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
            Some(Ok(ExecutionOutcome::Yielded(yielded))) => ReportOutcome::YieldToChild {
                checkpoint: yielded.checkpoint,
                child_environment_id: yielded.child.environment_id,
                child_title: yielded.child.title,
                child_input: yielded.child.input,
                resources_released: true,
            },
            Some(Ok(ExecutionOutcome::Completed(summary))) => ReportOutcome::Finished {
                success: summary.status() == crate::session::SessionStatus::Completed,
                result: json!({"version":1,"summary":summary,"text":snapshot.result_text,"truncated":snapshot.result_truncated}),
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
            .outcome_with_checkpoint(entry, report, local_checkpoint)
    }

    async fn flush_report(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
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
