//! Executions hosted by Desktop/CLI/TUI/legacy MCP still reserve the Hub's ordinary attempt.
use super::super::{RunnerCommand, RunnerResponse};
use super::*;
use camino::Utf8PathBuf;
use std::sync::Arc;
use ulid::Ulid;

#[derive(Clone, Serialize, Deserialize)]
pub struct LocalHuman {
    pub actor_device_id: String,
    pub principal_session: String,
    pub project_id: String,
}
impl std::fmt::Debug for LocalHuman {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHuman")
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ResourceCaller {
    Local,
    LegacyRemote,
}
fn participant_human(
    caller: ResourceCaller,
    local: Option<LocalHuman>,
) -> Result<Option<LocalHuman>, RunnerError> {
    if matches!(caller, ResourceCaller::LegacyRemote) {
        return Err(RunnerError::new(
            "Legacy remote access cannot use the server operator's Hub identity. Submit a Hub shared job with the calling user's authenticated session and project.",
        ));
    }
    Ok(local)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalAcquire {
    pub request_id: Ulid,
    pub environment_id: String,
    pub directory: Utf8PathBuf,
    pub human: Option<LocalHuman>,
    pub source: String,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalFinish {
    pub attempt_id: String,
    pub generation: u64,
    pub success: bool,
    pub result: serde_json::Value,
}
pub(crate) enum ExternalRequest {
    Acquire(
        ExternalAcquire,
        tokio::sync::oneshot::Sender<Result<Assignment, RunnerError>>,
    ),
    Finish(
        ExternalFinish,
        tokio::sync::oneshot::Sender<Result<(), RunnerError>>,
    ),
    Status(
        String,
        u64,
        tokio::sync::oneshot::Sender<Result<AttemptStatus, RunnerError>>,
    ),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ExternalEvidence {
    pub request_id: Ulid,
}

impl Controller {
    pub(super) async fn device_principal(
        &mut self,
        project: &str,
    ) -> Result<LocalHuman, RunnerError> {
        #[derive(Deserialize)]
        struct DeviceSession {
            token: String,
        }
        let session: DeviceSession = self.client
            .request("/v1/shared/device-session", Some(&json!({})))
            .await
            .map_err(|error| match error {
                TransportError::Rejected(status) if matches!(status.as_u16(), 404 | 405) =>
                    RunnerError::new("Update the Hub to use this approved device without a password"),
                TransportError::Rejected(status) if status.as_u16() == 403 =>
                    RunnerError::new("Ask the Hub administrator to approve and associate this device before selecting a project"),
                error => error.into(),
            })?;
        Ok(LocalHuman {
            actor_device_id: self.settings.device_id.clone(),
            principal_session: session.token,
            project_id: project.to_owned(),
        })
    }

    pub(super) async fn process_external(&mut self) {
        if let Some(request) = self
            .external
            .as_mut()
            .and_then(|queue| queue.try_recv().ok())
        {
            match request {
                ExternalRequest::Acquire(request, reply) => {
                    let result = self.acquire_external(request).await;
                    let _ = reply.send(result);
                }
                ExternalRequest::Finish(request, reply) => {
                    let result = self.finish_external(request).await;
                    let _ = reply.send(result);
                }
                ExternalRequest::Status(id, generation, reply) => {
                    let result = async {
                        let status = self.client.attempt(&id).await?;
                        if status.assignment.generation != generation {
                            return Err(RunnerError::new("Resource attempt generation changed"));
                        }
                        if status.state == "running"
                            && !status.assignment.stop_requested
                            && status.uncertainty_reason.is_none()
                        {
                            self.client.authorize(&status.assignment, None).await?;
                        }
                        Ok(status)
                    }
                    .await;
                    let _ = reply.send(result);
                }
            }
        }
    }
    async fn acquire_external(
        &mut self,
        request: ExternalAcquire,
    ) -> Result<Assignment, RunnerError> {
        if self.closing() || !self.host.accepting_new_shared() {
            return Err(RunnerError::new(
                "Resource provider is not accepting new work",
            ));
        }
        let environments = self.validated_resource_catalog().await?;
        if !crate::runtime::resource_admission::request_is_live(request.request_id)? {
            return Err(RunnerError::new(
                "Local execution has no live ownership lease",
            ));
        }
        let human = match request.human {
            Some(principal) => principal,
            None => {
                let project = self.local_project_id.clone().ok_or_else(|| RunnerError::new(
                    "Select an allowed Hub project with moyai-runner use-project before using this published resource",
                ))?;
                self.device_principal(&project).await?
            }
        };
        let mapping = local_mapping(
            &self.settings,
            &environments,
            &request.environment_id,
            &human.project_id,
        )?
        .clone();
        if !request.directory.is_absolute() {
            return Err(RunnerError::new(
                "Local resource directory must be absolute",
            ));
        }
        if matches!(
            self.settings.resource_scope,
            ResourceScope::WorkspaceIsolation { .. }
        ) && !crate::workspace::PathGuard::security_path_is_within(
            &request.directory,
            &mapping.directory,
        )
        .map_err(|e| RunnerError::new(e.to_string()))?
        {
            return Err(RunnerError::new(
                "This isolated resource does not cover the requested directory",
            ));
        }
        let assignment:Assignment=self.client.request("/v1/shared/runner/local-admissions",Some(&json!({"request_id":request.request_id,"environment_id":mapping.environment_id,"project_id":human.project_id,"actor_device_id":human.actor_device_id,"principal_session":human.principal_session,"source":request.source,"title":request.title}))).await?;
        self.validate_assignment(&assignment)?;
        let mut entry = self.journal.intent(assignment.clone(), mapping)?;
        if entry.phase == Phase::Intent {
            self.journal
                .external_execution(&mut entry, request.request_id)?;
        }
        if entry.phase != Phase::Executing
            || entry
                .external
                .as_ref()
                .is_none_or(|receipt| receipt.request_id != request.request_id)
        {
            return Err(RunnerError::new(
                "This local request already has uncertain or completed execution evidence",
            ));
        }
        Ok(assignment)
    }
    async fn finish_external(&mut self, request: ExternalFinish) -> Result<(), RunnerError> {
        let mut entry = self
            .journal
            .get(&request.attempt_id)?
            .ok_or_else(|| RunnerError::new("Local resource receipt is missing"))?;
        if entry.external.is_none() || entry.assignment.generation != request.generation {
            return Err(RunnerError::new("Local resource receipt changed"));
        }
        let report = Report::for_assignment(
            &entry.assignment,
            "external_outcome",
            ReportOutcome::Finished {
                success: request.success,
                result: request.result,
                resources_released: true,
            },
        );
        if entry.phase == Phase::Executing {
            self.journal.outcome(&mut entry, report.clone())?;
        }
        if entry.report.as_ref() != Some(&report) {
            return Err(RunnerError::new("The local resource outcome changed"));
        }
        if entry.phase == Phase::Settled {
            return Ok(());
        }
        self.flush_report(&mut entry).await
    }
}

fn local_mapping<'a>(
    settings: &'a SharedSettings,
    environments: &[super::provisioning::Environment],
    requested: &str,
    project: &str,
) -> Result<&'a EnvironmentMapping, RunnerError> {
    let permitted = |mapping: &&EnvironmentMapping| {
        environments.iter().any(|environment| {
            environment.id == mapping.environment_id
                && environment.enabled
                && environment.project_ids.iter().any(|id| id == project)
        })
    };
    match settings.resource_scope {
        ResourceScope::Device => settings.environments.iter().find(permitted),
        ResourceScope::WorkspaceIsolation { .. } => settings.environments.iter()
            .find(|mapping| mapping.environment_id == requested && permitted(mapping)),
    }.ok_or_else(|| RunnerError::new("No currently enabled published environment on this resource belongs to the selected project"))
}

impl RunnerHost {
    pub(crate) async fn dispatch_external(
        &self,
        command: RunnerCommand,
    ) -> Result<RunnerResponse, RunnerError> {
        let sender = self
            .inner
            .state
            .lock()
            .map_err(|_| RunnerError::new("Runner unavailable"))?
            .external_commands
            .clone()
            .ok_or_else(|| RunnerError::new("Shared resource provider is not configured"))?;
        match command {
            RunnerCommand::ExternalAcquire { request, .. } => {
                let (reply, receive) = tokio::sync::oneshot::channel();
                sender
                    .try_send(ExternalRequest::Acquire(request, reply))
                    .map_err(|_| RunnerError::new("Resource command capacity reached"))?;
                Ok(RunnerResponse::ResourceAssignment {
                    assignment: receive
                        .await
                        .map_err(|_| RunnerError::new("Resource admission interrupted"))??,
                })
            }
            RunnerCommand::ExternalFinish { request, .. } => {
                let (reply, receive) = tokio::sync::oneshot::channel();
                sender
                    .try_send(ExternalRequest::Finish(request, reply))
                    .map_err(|_| RunnerError::new("Resource command capacity reached"))?;
                receive
                    .await
                    .map_err(|_| RunnerError::new("Resource completion interrupted"))??;
                Ok(RunnerResponse::ResourceFinished)
            }
            RunnerCommand::ExternalStatus {
                attempt_id,
                generation,
                ..
            } => {
                let (reply, receive) = tokio::sync::oneshot::channel();
                sender
                    .try_send(ExternalRequest::Status(attempt_id, generation, reply))
                    .map_err(|_| RunnerError::new("Resource command capacity reached"))?;
                Ok(RunnerResponse::ResourceStatus {
                    status: receive
                        .await
                        .map_err(|_| RunnerError::new("Resource status interrupted"))??,
                })
            }
            _ => Err(RunnerError::new("Invalid resource command")),
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalResourceLease(Arc<LeaseInner>);
struct LeaseInner {
    config: Utf8PathBuf,
    runner: Ulid,
    assignment: Assignment,
    _file: std::fs::File,
    _resource: crate::runtime::resource_admission::ResourceGuard,
    monitor_stop: tokio_util::sync::CancellationToken,
    monitor_finished: tokio_util::sync::CancellationToken,
}
struct MonitorFinished(tokio_util::sync::CancellationToken);
impl Drop for MonitorFinished {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl LocalResourceLease {
    pub(crate) fn watch(
        &self,
        service: Arc<crate::app::RunService>,
        control: crate::runtime::RunControl,
    ) -> impl std::future::Future<Output = ()> + 'static {
        let weak = Arc::downgrade(&self.0);
        let stop = self.0.monitor_stop.clone();
        // Created outside the future so abandoning an unpolled worker also acknowledges it.
        let finished = MonitorFinished(self.0.monitor_finished.clone());
        async move {
            let _finished = finished;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                let Some(lease) = weak.upgrade() else {
                    break;
                };
                let config = lease.config.clone();
                let runner = lease.runner;
                let attempt = lease.assignment.attempt_id.clone();
                let generation = lease.assignment.generation;
                drop(lease);
                #[cfg(windows)]
                let result = tokio::task::spawn_blocking(move || {
                    super::super::windows::request_for_config(
                        &RunnerCommand::ExternalStatus {
                            runner_id: runner,
                            attempt_id: attempt,
                            generation,
                        },
                        &config,
                    )
                })
                .await;
                #[cfg(not(windows))]
                let result: Result<
                    Result<RunnerResponse, RunnerError>,
                    tokio::task::JoinError,
                > = Ok(Err(RunnerError::new(
                    "Shared resource IPC supports Windows only",
                )));
                if stop.is_cancelled() {
                    break;
                }
                // Keep the exact resource lifetime alive through the stop operation. Finish
                // awaits this worker before releasing the Hub slot or admitting a replacement.
                let Some(_lifetime) = weak.upgrade() else {
                    break;
                };
                if !matches!(result,Ok(Ok(RunnerResponse::ResourceStatus {status})) if status.state=="running" && !status.assignment.stop_requested && status.uncertainty_reason.is_none())
                {
                    // The existing root stop owner cancels descendants and pending permission
                    // waits. Receipt release still waits for all managed processes below it.
                    service.stop_local_resource_processes(&control);
                    let _ = service.request_root_execution_stop(&control).await;
                    control.cancel(crate::runtime::RunCancellationCause::Interruption(
                        crate::protocol::TurnInterruptionCause::UserStop,
                    ));
                    break;
                }
            }
        }
    }
    #[cfg(windows)]
    pub(crate) async fn acquire(
        directory: &camino::Utf8Path,
        store: &crate::storage::StoreBundle,
        control: &crate::runtime::RunControl,
        title: &str,
        caller: ResourceCaller,
    ) -> Result<Option<Self>, RunnerError> {
        let Some(binding) = crate::runtime::resource_admission::for_workspace(directory)? else {
            return Ok(None);
        };
        if binding.operator_sid != super::super::windows::current_user_sid()? {
            return Err(RunnerError::new(
                "This Windows device provides shared resources under another OS account. Submit a Hub shared job to its Runner; direct Desktop, CLI, TUI, and legacy MCP execution from this account is unavailable.",
            ));
        }
        let human = participant_human(
            caller,
            store
                .device_network()
                .and_then(|network| network.local_resource_human()),
        )?;
        let request_id = Ulid::new();
        let file = crate::runtime::resource_admission::request_lock(request_id)?;
        let config = binding.config_path;
        let resource_scope = binding.scope.clone();
        let resource_directory = binding.directory.clone();
        let target = config.clone();
        let directory = directory.to_owned();
        let title = title.chars().take(256).collect::<String>();
        let (runner, assignment) = tokio::task::spawn_blocking(move || {
            let RunnerResponse::Identity { identity } =
                super::super::windows::request_for_config(&RunnerCommand::Identity, &target)?
            else {
                return Err(RunnerError::new("Unexpected resource provider identity"));
            };
            let response = super::super::windows::request_for_config(
                &RunnerCommand::ExternalAcquire {
                    runner_id: identity.runner_id,
                    request: ExternalAcquire {
                        request_id,
                        environment_id: binding.environment_id,
                        directory,
                        human,
                        source: "local".into(),
                        title,
                    },
                },
                &target,
            )?;
            let RunnerResponse::ResourceAssignment { assignment } = response else {
                return Err(RunnerError::new("Unexpected resource admission response"));
            };
            Ok((identity.runner_id, assignment))
        })
        .await
        .map_err(|_| RunnerError::new("Resource admission worker stopped"))??;
        // The Hub reservation is durable before this final physical-device gate is acquired.
        // Failure leaves an unknown reservation, never a speculative local execution.
        let resource = crate::runtime::resource_admission::ResourceGuard::acquire(
            &resource_scope,
            &resource_directory,
        )?;
        let lease = Self(Arc::new(LeaseInner {
            config,
            runner,
            assignment,
            _file: file,
            _resource: resource,
            monitor_stop: tokio_util::sync::CancellationToken::new(),
            monitor_finished: tokio_util::sync::CancellationToken::new(),
        }));
        control.set_effect_authority(Arc::new(LocalResourceAuthority(Arc::downgrade(&lease.0))));
        Ok(Some(lease))
    }
    #[cfg(not(windows))]
    pub(crate) async fn acquire(
        _: &camino::Utf8Path,
        _: &crate::storage::StoreBundle,
        _: &crate::runtime::RunControl,
        _: &str,
        _: ResourceCaller,
    ) -> Result<Option<Self>, RunnerError> {
        Ok(None)
    }
    pub(crate) async fn finish(
        self,
        success: bool,
        result: serde_json::Value,
    ) -> Result<(), RunnerError> {
        self.0.monitor_stop.cancel();
        self.0.monitor_finished.cancelled().await;
        #[cfg(windows)]
        {
            let this = self.clone();
            tokio::task::spawn_blocking(move || {
                let response = super::super::windows::request_for_config(
                    &RunnerCommand::ExternalFinish {
                        runner_id: this.0.runner,
                        request: ExternalFinish {
                            attempt_id: this.0.assignment.attempt_id.clone(),
                            generation: this.0.assignment.generation,
                            success,
                            result,
                        },
                    },
                    &this.0.config,
                )?;
                if !matches!(response, RunnerResponse::ResourceFinished) {
                    return Err(RunnerError::new("Unexpected resource completion response"));
                }
                Ok(())
            })
            .await
            .map_err(|_| RunnerError::new("Resource completion worker stopped"))?
        }
        #[cfg(not(windows))]
        {
            let _ = (success, result);
            Err(RunnerError::new(
                "Shared resource IPC supports Windows only",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_local_routing_uses_the_selected_project_and_live_environment() {
        let mappings = ["alpha", "beta-disabled", "beta"].map(|id| EnvironmentMapping {
            environment_id: id.into(),
            directory: Utf8PathBuf::from(format!("C:/fixture/{id}")),
            access_mode: crate::config::AccessMode::Default,
            allowed_child_environments: vec![],
        });
        let mut settings = SharedSettings {
            version: 1,
            hub_id: "hub".into(),
            device_id: "device".into(),
            resource_scope: ResourceScope::Device,
            environments: mappings.into(),
        };
        let catalog = [
            ("alpha", "Alpha", true),
            ("beta-disabled", "Beta", false),
            ("beta", "Beta", true),
        ]
        .map(
            |(id, project, enabled)| super::super::provisioning::Environment {
                id: id.into(),
                resource_id: "physical-device".into(),
                capacity: 1,
                project_ids: vec![project.into()],
                enabled,
            },
        );
        assert_eq!(
            local_mapping(&settings, &catalog, "alpha", "Beta")
                .unwrap()
                .environment_id,
            "beta"
        );
        assert!(local_mapping(&settings, &catalog, "alpha", "Gamma").is_err());
        assert!(local_mapping(&settings, &catalog[..2], "alpha", "Beta").is_err());
        settings.resource_scope = ResourceScope::WorkspaceIsolation { confirmed: true };
        assert_eq!(
            local_mapping(&settings, &catalog, "alpha", "Alpha")
                .unwrap()
                .environment_id,
            "alpha"
        );
        assert!(local_mapping(&settings, &catalog, "alpha", "Beta").is_err());
        assert!(local_mapping(&settings, &catalog, "beta-disabled", "Beta").is_err());
    }
    #[test]
    fn legacy_remote_cannot_borrow_the_server_operators_hub_identity() {
        let server = LocalHuman {
            actor_device_id: "server".into(),
            principal_session: "server-operators-session".into(),
            project_id: "operators-project".into(),
        };
        assert!(participant_human(ResourceCaller::LegacyRemote, Some(server.clone())).is_err());
        assert!(participant_human(ResourceCaller::LegacyRemote, None).is_err());
        assert_eq!(
            participant_human(ResourceCaller::Local, Some(server))
                .unwrap()
                .unwrap()
                .project_id,
            "operators-project"
        );
        assert!(
            participant_human(ResourceCaller::Local, None)
                .unwrap()
                .is_none()
        );
    }
}
impl std::fmt::Debug for LocalResourceLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalResourceLease")
            .field("attempt", &self.0.assignment.attempt_id)
            .finish()
    }
}
impl crate::runtime::ExternalEffectAuthority for LocalResourceLease {
    fn authorize(&self, _: Option<&str>) -> Result<(), String> {
        #[cfg(windows)]
        {
            let response = super::super::windows::request_for_config(
                &RunnerCommand::ExternalStatus {
                    runner_id: self.0.runner,
                    attempt_id: self.0.assignment.attempt_id.clone(),
                    generation: self.0.assignment.generation,
                },
                &self.0.config,
            )
            .map_err(|e| e.message)?;
            match response {
                RunnerResponse::ResourceStatus { status }
                    if status.state == "running"
                        && !status.assignment.stop_requested
                        && status.uncertainty_reason.is_none() =>
                {
                    Ok(())
                }
                _ => Err("The shared resource authority changed before this effect".into()),
            }
        }
        #[cfg(not(windows))]
        {
            Err("Shared resource IPC supports Windows only".into())
        }
    }
}

struct LocalResourceAuthority(std::sync::Weak<LeaseInner>);
impl std::fmt::Debug for LocalResourceAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalResourceAuthority")
    }
}
impl crate::runtime::ExternalEffectAuthority for LocalResourceAuthority {
    fn authorize(&self, approval: Option<&str>) -> Result<(), String> {
        let lease = self
            .0
            .upgrade()
            .ok_or_else(|| "Resource execution lifetime has ended".to_string())?;
        crate::runtime::ExternalEffectAuthority::authorize(&LocalResourceLease(lease), approval)
    }
}
