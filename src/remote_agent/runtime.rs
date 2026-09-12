//! A small receiver adapter around the existing process runtime and RunService.
//! HTTP request lifetimes never own an accepted job's lifetime.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use super::store::{RemoteJobParent, RemoteTaskRequest, StoredRemoteJob};
use crate::app::{
    App, AppBootstrap, AppCommand, AppProcessRuntime, RunAdmissionKind, RunConfigInput, RunRequest,
    RunService,
};
use crate::cli::{EventRenderer, OutputMode, SharedConfirmationPrompt};
use crate::config::ResolvedConfig;
use crate::error::CliRenderError;
use crate::mcp_publish::dispatch::{PublishCallError, PublishToolDispatcher, TargetSnapshot};
use crate::mcp_publish::{PublishMode, PublishProfile, PublishProfileId, PublishTarget};
use crate::protocol::{ContentPart, HistoryItem, ReviewDecision};
use crate::runtime::{LocalTaskExecutor, OwnedTaskHandle, RunControl};
use crate::session::{
    ActiveTurnExpectation, NewSession, SessionId, SessionProviderConnection, SessionRepository,
    SessionStatus,
};

const MAX_ACTIVE_JOBS: usize = 16;
const MAX_RESULT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteJobState {
    Accepted,
    Running,
    AwaitingApproval,
    Cancelling,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteJobRow {
    pub job_id: Ulid,
    pub profile_id: PublishProfileId,
    pub parent: RemoteJobParent,
    pub prompt_preview: String,
    pub session_id: SessionId,
    pub state: RemoteJobState,
    pub model: String,
    pub result: Option<String>,
    pub result_truncated: bool,
    pub can_stop: bool,
    pub network: Option<RemoteJobNetwork>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteJobNetwork {
    pub origin_device_id: String,
    pub actor_device_id: String,
    pub root_task_id: String,
    pub device_path: Vec<String>,
}

#[derive(Clone)]
pub struct RemoteJobService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    process: AppProcessRuntime,
    executor: LocalTaskExecutor,
    state: Mutex<RuntimeState>,
    approvals: super::approval::ReceiverApprovals,
}

#[derive(Default)]
struct RuntimeState {
    profiles: HashMap<PublishProfileId, Arc<ProfileScope>>,
    workers: HashMap<SessionId, JobWorker>,
}

struct ProfileScope {
    profile: PublishProfile,
    app: App,
    target: PublishTarget,
    target_snapshot: TargetSnapshot,
    scope_json: String,
    admission_closed: CancellationToken,
    accepting: std::sync::atomic::AtomicBool,
    network: Option<crate::device_network::WeakDeviceNetwork>,
    _temporary_workspace: Option<tempfile::TempDir>,
}

struct JobWorker {
    profile: PublishProfileId,
    run_control: RunControl,
    run_service: Arc<RunService>,
    managed_shell_lifetime: CancellationToken,
    handle: OwnedTaskHandle,
}

impl RuntimeState {
    fn reap(&mut self) {
        self.workers
            .retain(|_, worker| !worker.handle.is_finished());
    }
}

impl RemoteJobService {
    pub(super) fn history_store(&self) -> crate::storage::StoreBundle {
        self.inner.process.store()
    }

    pub(crate) fn new(process: AppProcessRuntime) -> Result<Self, PublishCallError> {
        Ok(Self {
            inner: Arc::new(ServiceInner {
                process,
                executor: LocalTaskExecutor::new("moyai-remote-job-runtime")
                    .map_err(|_| PublishCallError::Unavailable)?,
                state: Mutex::new(RuntimeState::default()),
                approvals: super::approval::ReceiverApprovals::default(),
            }),
        })
    }

    pub(crate) fn pending_approval(&self) -> Option<super::approval::RemoteApproval> {
        self.inner.approvals.pending()
    }

    pub(crate) fn answer_approval(
        &self,
        id: Ulid,
        job: Ulid,
        profile: PublishProfileId,
        decision: ReviewDecision,
    ) -> bool {
        self.inner.approvals.answer(id, job, profile, decision)
    }

    pub(crate) fn has_active_jobs(&self) -> bool {
        self.inner.state.lock().is_ok_and(|mut state| {
            state.reap();
            !state.workers.is_empty()
        })
    }

    /// Synchronous snapshot for Desktop's existing ordered projection. At most
    /// MAX_ACTIVE_JOBS job mappings are read; result bodies/history are never loaded.
    pub(crate) fn activity_now(&self) -> super::RemoteActivityProjection {
        let result = (|| {
            let workers = {
                let state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| PublishCallError::Unavailable)?;
                state
                    .workers
                    .iter()
                    .filter(|(_, worker)| !worker.handle.is_finished())
                    .map(|(session, worker)| {
                        (
                            *session,
                            worker.run_control.is_cancelled()
                                || state
                                    .profiles
                                    .get(&worker.profile)
                                    .is_some_and(|scope| scope.admission_closed.is_cancelled()),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let store = self.inner.process.store().remote_job_store();
            let mut states = Vec::with_capacity(workers.len());
            for (session, cancelling) in workers {
                let job = store
                    .job_for_session(session)
                    .map_err(|_| PublishCallError::Unavailable)?
                    .ok_or(PublishCallError::Unavailable)?;
                states.push(if cancelling {
                    RemoteJobState::Cancelling
                } else if self.inner.approvals.waiting(job.id) {
                    RemoteJobState::AwaitingApproval
                } else if job.admitted_turn_id.is_some() {
                    RemoteJobState::Running
                } else {
                    RemoteJobState::Accepted
                });
            }
            Ok::<_, PublishCallError>(super::RemoteActivityProjection::from_states(states))
        })();
        result.unwrap_or(super::RemoteActivityProjection {
            unavailable: true,
            ..Default::default()
        })
    }

    /// Delivery retries never resume a local turn. They only reconcile the receiver's
    /// durable job mapping/terminal with Hub after a lost response or Hub restart.
    pub(crate) async fn reconcile_network_receipts(
        &self,
        network: crate::device_network::DeviceNetworkService,
    ) {
        let _ = self
            .submit(move |service| async move {
                let receipts = service
                    .inner
                    .process
                    .store()
                    .remote_job_store()
                    .pending_network_receipts(8)
                    .map_err(|_| PublishCallError::Unavailable)?;
                for (job, grant_id) in receipts {
                    let _ = service
                        .reconcile_network_receipt(&network, job, &grant_id)
                        .await;
                }
                Ok(())
            })
            .await;
    }

    async fn reconcile_network_receipt(
        &self,
        network: &crate::device_network::DeviceNetworkService,
        job: StoredRemoteJob,
        grant_id: &str,
    ) -> Result<(), PublishCallError> {
        let id = job.id;
        let row = self.row(job.clone()).await?;
        let state = match row.state {
            RemoteJobState::Completed => Some("completed"),
            RemoteJobState::Failed => Some("failed"),
            RemoteJobState::Interrupted => Some("cancelled"),
            _ => None,
        };
        let delivered = if let Some(state) = state {
            let value: Value =
                serde_json::from_str(&job.scope_json).map_err(|_| PublishCallError::Unavailable)?;
            let claims = serde_json::from_value(
                value
                    .get("network")
                    .cloned()
                    .ok_or(PublishCallError::InvalidTarget)?,
            )
            .map_err(|_| PublishCallError::InvalidTarget)?;
            let authority = crate::device_network::VerifiedGrant {
                grant_id: grant_id.into(),
                claims,
            };
            // Covers a lost accepted response without ever admitting or replaying execution.
            network
                .settle_incoming_job(grant_id, &id.to_string(), state)
                .await
                .is_ok()
                || (network
                    .acknowledge_incoming_job(&authority, &id.to_string())
                    .await
                    .is_ok()
                    && network
                        .settle_incoming_job(grant_id, &id.to_string(), state)
                        .await
                        .is_ok())
        } else {
            false
        };
        self.inner
            .process
            .store()
            .remote_job_store()
            .network_receipt_attempt(id, grant_id, delivered)
            .map_err(|_| PublishCallError::Unavailable)
    }

    /// `config` is the receiver's configuration. It is never supplied by the network caller.
    pub(crate) async fn dispatcher(
        &self,
        profile: PublishProfile,
        config: ResolvedConfig,
        protected_roots: Vec<Utf8PathBuf>,
    ) -> Result<Arc<dyn PublishToolDispatcher>, PublishCallError> {
        self.dispatcher_with_network(profile, config, protected_roots, None)
            .await
    }

    pub(crate) async fn dispatcher_network(
        &self,
        profile: PublishProfile,
        config: ResolvedConfig,
        protected_roots: Vec<Utf8PathBuf>,
        network: crate::device_network::WeakDeviceNetwork,
    ) -> Result<Arc<dyn PublishToolDispatcher>, PublishCallError> {
        self.dispatcher_with_network(profile, config, protected_roots, Some(network))
            .await
    }

    async fn dispatcher_with_network(
        &self,
        profile: PublishProfile,
        config: ResolvedConfig,
        protected_roots: Vec<Utf8PathBuf>,
        network: Option<crate::device_network::WeakDeviceNetwork>,
    ) -> Result<Arc<dyn PublishToolDispatcher>, PublishCallError> {
        self.submit(move |service| async move {
            profile.validate().map_err(|_| PublishCallError::InvalidTarget)?;
            let PublishMode::Agent { access_mode } = profile.mode else { return Err(PublishCallError::InvalidTarget); };
            if matches!(profile.target, PublishTarget::LegacySession { .. }) { return Err(PublishCallError::InvalidTarget); }
            let mut config = config;
            config.permissions.access_mode = access_mode;
            config.permissions.additional_read_roots.clear();
            config.permissions.additional_write_roots.clear();
            config.workspace.protected_paths.extend(protected_roots);
            config.workspace.protected_paths.push(service.inner.process.store().paths().data_dir.clone());
            // The intermediate receiver runs one root. It cannot recursively delegate or
            // silently turn one job slot into another device/agent tree.
            config.multi_agent.enabled = false;
            config.mcp.enabled = false;
            config.mcp.servers.clear();
            let temporary = if matches!(profile.target, PublishTarget::Temp {}) {
                Some(tempfile::Builder::new().prefix("moyai-remote-").tempdir().map_err(|_| PublishCallError::InvalidTarget)?)
            } else { None };
            let root = if let Some(directory) = &temporary {
                Utf8PathBuf::from_path_buf(directory.path().to_path_buf()).map_err(|_| PublishCallError::InvalidTarget)?
            } else {
                // Verify the registered object before bootstrap can upsert project metadata.
                TargetSnapshot::capture(&service.inner.process.store(), &profile.target, &config).await?;
                profile.target.workspace_root().cloned().ok_or(PublishCallError::InvalidTarget)?
            };
            let app = if temporary.is_some() {
                AppBootstrap::rebuild_for_remote_temp_with_process_runtime_and_config(
                    &root, service.inner.process.clone(), config, profile.id.0,
                ).await
            } else {
                AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
                    &root, service.inner.process.clone(), config,
                ).await
            }.map_err(|_| PublishCallError::InvalidTarget)?;
            let target = match profile.target {
                PublishTarget::Temp {} => PublishTarget::Project { project_id: app.workspace.project_id, workspace_root: app.workspace.root.clone() },
                _ => profile.target.clone(),
            };
            let target_snapshot = TargetSnapshot::capture(&app.store, &target, &app.config).await?;
            if target_snapshot.workspace().is_none_or(|workspace| workspace.project_id != app.workspace.project_id) {
                return Err(PublishCallError::InvalidTarget);
            }
            // Credentials do not belong to request identity. A credential rotation must
            // still find a job accepted by the same authenticated profile principal.
            let scope_json = serde_json::to_string(&json!({
                "profile_id":profile.id, "target":profile.target, "access_mode":access_mode,
                "model":app.config.model.model, "provider_profile":app.config.model.provider_profile,
                "base_url":app.config.model.base_url,
            })).map_err(|_| PublishCallError::InvalidTarget)?;
            let scope = Arc::new(ProfileScope { profile, app, target, target_snapshot, scope_json,
                admission_closed: CancellationToken::new(), accepting: std::sync::atomic::AtomicBool::new(true),
                network, _temporary_workspace: temporary });
            {
                let mut state = service.inner.state.lock().map_err(|_| PublishCallError::Unavailable)?;
                state.reap();
                if state.workers.values().any(|worker| worker.profile == scope.profile.id)
                    || state.profiles.get(&scope.profile.id).is_some_and(|old| !old.admission_closed.is_cancelled()) {
                    return Err(PublishCallError::Unavailable);
                }
                state.profiles.insert(scope.profile.id, scope.clone());
            }
            Ok(Arc::new(RemoteDispatcher { service, scope }) as Arc<dyn PublishToolDispatcher>)
        }).await
    }

    pub(crate) fn has_active_profile(&self, profile: PublishProfileId) -> bool {
        let mut state = self.inner.state.lock().expect("remote state poisoned");
        state.reap();
        state
            .workers
            .values()
            .any(|worker| worker.profile == profile)
    }
    pub(crate) fn network_scope_id(
        &self,
        profile: PublishProfileId,
    ) -> Result<String, PublishCallError> {
        use sha2::{Digest, Sha256};
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| PublishCallError::Unavailable)?;
        let scope = state
            .profiles
            .get(&profile)
            .ok_or(PublishCallError::InvalidTarget)?;
        Ok(format!("{:x}", Sha256::digest(scope.scope_json.as_bytes())))
    }
    pub(crate) fn set_network_accepting(&self, profile: PublishProfileId, accepting: bool) {
        if let Ok(state) = self.inner.state.lock() {
            if let Some(scope) = state
                .profiles
                .get(&profile)
                .filter(|scope| scope.network.is_some())
            {
                scope
                    .accepting
                    .store(accepting, std::sync::atomic::Ordering::Release);
            }
        }
    }

    // The closure constructs local futures on the existing !Send executor. Transport callers
    // remain Send without changing the tool/storage traits or adding an alternate agent loop.
    pub(super) async fn submit<F, Fut, T>(&self, operation: F) -> Result<T, PublishCallError>
    where
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, PublishCallError>> + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let service = self.clone();
        let handle = self
            .inner
            .executor
            .spawn(0, move || async move {
                let result = operation(service).await;
                let _ = tx.send(result);
            })
            .map_err(|_| PublishCallError::Unavailable)?;
        let result = rx.await.map_err(|_| PublishCallError::Unavailable)?;
        // Completion sent immediately before task return; retaining ownership until here
        // also cancels an unaccepted command if its transport caller disappears.
        handle.detach();
        result
    }

    pub(crate) async fn rows_all(&self) -> Result<Vec<RemoteJobRow>, PublishCallError> {
        self.submit(move |service| async move {
            let jobs = service
                .inner
                .process
                .store()
                .remote_job_store()
                .recent_all(64)
                .map_err(|_| PublishCallError::Unavailable)?;
            let mut rows = Vec::with_capacity(jobs.len());
            for job in jobs {
                let mut row = service.row(job).await?;
                if let Some(text) = &mut row.result {
                    if text.len() > 512 {
                        let mut limit = 512;
                        while !text.is_char_boundary(limit) {
                            limit -= 1;
                        }
                        text.truncate(limit);
                        row.result_truncated = true;
                    }
                }
                rows.push(row);
            }
            Ok(rows)
        })
        .await
    }

    pub(crate) async fn cancel_job(
        &self,
        profile: PublishProfileId,
        job_id: Ulid,
    ) -> Result<RemoteJobRow, PublishCallError> {
        self.submit(move |service| async move { service.cancel_job_inner(profile, job_id).await })
            .await
    }

    async fn cancel_job_inner(
        &self,
        profile: PublishProfileId,
        job_id: Ulid,
    ) -> Result<RemoteJobRow, PublishCallError> {
        let job = self
            .inner
            .process
            .store()
            .remote_job_store()
            .get_for_profile(profile.0, job_id)
            .map_err(|_| PublishCallError::Unavailable)?
            .ok_or(PublishCallError::InvalidTarget)?;
        let captured = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| PublishCallError::Unavailable)?;
            state.reap();
            state
                .workers
                .get(&job.session_id)
                .map(|worker| (worker.run_service.clone(), worker.run_control.clone()))
        };
        if let Some((run_service, control)) = captured {
            run_service
                .request_root_execution_stop(&control)
                .await
                .map_err(|_| PublishCallError::Unavailable)?;
        }
        self.row(job).await
    }

    pub(crate) fn cancel_profile(&self, profile: PublishProfileId) {
        let captured = {
            let Ok(mut state) = self.inner.state.lock() else {
                self.inner
                    .process
                    .managed_shells()
                    .cancel_profile(profile.0);
                return;
            };
            state.reap();
            if let Some(scope) = state.profiles.get(&profile) {
                scope.admission_closed.cancel();
            }
            state
                .workers
                .values()
                .filter(|worker| worker.profile == profile)
                .map(|worker| (worker.run_service.clone(), worker.run_control.clone()))
                .collect::<Vec<_>>()
        };
        // Completed jobs may still own a managed server process.
        self.inner
            .process
            .managed_shells()
            .cancel_profile(profile.0);
        // A stop operation is owned until it has requested the ordinary canonical Stop.
        // The job worker remains in the registry until it actually finishes.
        if let Ok(handle) = self.inner.executor.spawn(0, move || async move {
            for (service, control) in captured {
                let _ = service.request_root_execution_stop(&control).await;
            }
        }) {
            handle.detach();
        }
    }

    pub(crate) fn cancel_network_lineages(&self, lineages: Vec<(String, String)>) {
        if lineages.is_empty() {
            return;
        }
        let captured = {
            let Ok(state) = self.inner.state.lock() else {
                self.inner
                    .process
                    .managed_shells()
                    .cancel_lineages(&lineages);
                return;
            };
            let store = self.inner.process.store().remote_job_store();
            state
                .workers
                .iter()
                .filter_map(|(session, worker)| {
                    let job_id = store.job_id_for_session(*session).ok().flatten()?;
                    let job = store
                        .get_for_profile(worker.profile.0, job_id)
                        .ok()
                        .flatten()?;
                    let value = serde_json::from_str::<Value>(&job.scope_json).ok()?;
                    let claims = serde_json::from_value::<crate::device_network::GrantClaims>(
                        value.get("network")?.clone(),
                    )
                    .ok()?;
                    lineages
                        .iter()
                        .any(|(origin, root)| {
                            origin == &claims.origin_device_id && root == &claims.root_task_id
                        })
                        .then(|| {
                            // Close command admission synchronously with selecting the job.
                            // Canonical Stop below may settle after a shell_start resumes.
                            worker.managed_shell_lifetime.cancel();
                            (worker.run_service.clone(), worker.run_control.clone())
                        })
                })
                .collect::<Vec<_>>()
        };
        // A completed worker can be reaped before this lock is acquired. Its
        // command was already registered; scan after closing every active lifetime.
        self.inner
            .process
            .managed_shells()
            .cancel_lineages(&lineages);
        if let Ok(handle) = self.inner.executor.spawn(0, move || async move {
            for (service, control) in captured {
                let _ = service.request_root_execution_stop(&control).await;
            }
        }) {
            handle.detach();
        }
    }

    pub(crate) async fn drain_profile(&self, profile: PublishProfileId, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let managed_shells = self.inner.process.managed_shells();
        loop {
            let active = match self.inner.state.lock() {
                Ok(mut state) => {
                    state.reap();
                    state
                        .workers
                        .values()
                        .any(|worker| worker.profile == profile)
                }
                Err(_) => return false,
            };
            if !active && !managed_shells.has_profile_work(profile.0) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn start_job(
        &self,
        scope: &Arc<ProfileScope>,
        request: RemoteTaskRequest,
        authority: Option<crate::device_network::VerifiedGrant>,
        admission_cancel: CancellationToken,
    ) -> Result<RemoteJobRow, PublishCallError> {
        if !request.validate() {
            return Err(PublishCallError::InvalidArguments);
        }
        scope
            .target_snapshot
            .verify(&scope.app.store, &scope.target)
            .await?;
        let principal = authority
            .as_ref()
            .map(network_principal)
            .unwrap_or_else(|| scope.profile.id.0.to_string());
        let scope_json = match &authority {
            Some(authority) => serde_json::to_string(
                &json!({"receiver":scope.scope_json,"network":authority.claims()}),
            )
            .map_err(|_| PublishCallError::InvalidArguments)?,
            None => scope.scope_json.clone(),
        };
        let network = scope.network.as_ref().and_then(|network| network.upgrade());
        let existing = self
            .inner
            .process
            .store()
            .remote_job_store()
            .find_request(&principal, &request.request_key)
            .map_err(|_| PublishCallError::Unavailable)?;
        let control = RunControl::new();
        let route = if existing.is_none() {
            if let Some(network) = &network {
                network
                    .receiver_model_route(control.token())
                    .await
                    .map_err(|_| PublishCallError::Unavailable)?
            } else {
                None
            }
        } else {
            None
        };
        let mut run_config = route.as_ref().map_or_else(
            || scope.app.config.clone(),
            |route| route.runtime_config(&scope.app.config),
        );
        if existing.is_none()
            && let Some(authority) = &authority
        {
            let mut identity = network
                .as_ref()
                .ok_or(PublishCallError::Unavailable)?
                .execution_identity(authority)
                .map_err(|_| PublishCallError::Unavailable)?;
            identity["workspace_root"] = json!(scope.app.workspace.authority_root());
            identity["cwd"] = json!(scope.app.workspace.cwd);
            let context = include_str!("../../assets/prompts/remote_receiver.md")
                .replace("{{receiver}}", &identity.to_string());
            let original = run_config.model.system_prompt.trim_end();
            run_config.model.system_prompt = if original.is_empty() {
                context
            } else {
                format!("{original}\n\n{context}")
            };
        }
        let run_service = route.as_ref().map_or_else(
            || scope.app.run_service.clone(),
            |route| Arc::new(scope.app.run_service.with_hub_turn(route.clone())),
        );
        let job = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| PublishCallError::Unavailable)?;
            state.reap();
            if admission_cancel.is_cancelled()
                || scope.admission_closed.is_cancelled()
                || !scope.accepting.load(std::sync::atomic::Ordering::Acquire)
                || state
                    .profiles
                    .get(&scope.profile.id)
                    .is_none_or(|current| !Arc::ptr_eq(current, scope))
            {
                return Err(PublishCallError::Unavailable);
            }
            let store = self.inner.process.store().remote_job_store();
            if let Some(existing) = store
                .find_request(&principal, &request.request_key)
                .map_err(|_| PublishCallError::Unavailable)?
            {
                if request
                    .fingerprint(&scope_json)
                    .map_err(|_| PublishCallError::InvalidArguments)?
                    != existing.request_hash
                {
                    return Err(PublishCallError::InvalidArguments);
                }
                existing
            } else {
                if state.workers.len() >= MAX_ACTIVE_JOBS
                    || state
                        .workers
                        .values()
                        .any(|worker| worker.profile == scope.profile.id)
                {
                    return Err(PublishCallError::Unavailable);
                }
                let draft = NewSession {
                    project_id: scope.app.workspace.project_id,
                    title: "受信したエージェント作業".into(),
                    cwd: scope.app.workspace.cwd.clone(),
                    model: run_config.model.model.clone(),
                    base_url: run_config.model.base_url.clone(),
                    access_mode: run_config.permissions.access_mode,
                    provider_connection: Some(SessionProviderConnection::from_model_config(
                        &run_config.model,
                    )),
                };
                let (job, created) = store
                    .accept_with_network(
                        &principal,
                        scope.profile.id.0,
                        &request,
                        &scope_json,
                        &draft,
                        authority
                            .as_ref()
                            .map(|authority| authority.grant_id.as_str()),
                    )
                    .map_err(|_| PublishCallError::InvalidArguments)?;
                if created {
                    let worker_control = control.clone();
                    let worker_scope = scope.clone();
                    let id = job.id;
                    let session_id = job.session_id;
                    if let (Some(network), Some(authority)) = (&network, &authority) {
                        network.capture_inbound(session_id, job.id, authority.clone());
                    }
                    let managed_shell_lifetime = scope.admission_closed.child_token();
                    let worker_run_service = Arc::new(
                        run_service.with_managed_shell_lifetime(managed_shell_lifetime.clone()),
                    );
                    let worker_network = network.clone();
                    let worker_authority = authority.clone();
                    let worker_service = self.clone();
                    let worker_job = job.clone();
                    let receiver_prompt =
                        self.inner
                            .approvals
                            .prompt(super::approval::RemoteApprovalContext {
                                job_id: id,
                                profile_id: scope.profile.id,
                                session_id,
                                requester_label: Some(request.parent.peer_id.clone()),
                                target_label: scope
                                    .profile
                                    .target
                                    .workspace_root()
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| "temp".into()),
                            });
                    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
                    let handle = self
                        .inner
                        .executor
                        .spawn(0, move || async move {
                            if start_rx.await.is_err()
                                || worker_scope.admission_closed.is_cancelled()
                            {
                                return;
                            }
                            if let (Some(network), Some(authority)) =
                                (&worker_network, &worker_authority)
                            {
                                if network
                                    .acknowledge_incoming_job(authority, &id.to_string())
                                    .await
                                    .is_err()
                                    || worker_control.is_cancelled()
                                    || worker_scope.admission_closed.is_cancelled()
                                {
                                    if let Some(route) = route {
                                        route.finish().await;
                                    }
                                    return;
                                }
                            }
                            let inputs = match super::artifacts::stage_inputs(
                                &worker_scope.app.store.remote_job_store(),
                                &worker_job,
                            ) {
                                Ok(inputs) => inputs,
                                Err(_) => {
                                    if let Some(route) = route {
                                        route.finish().await;
                                    }
                                    return;
                                }
                            };
                            let worker_run_service = if let Some(inputs) = &inputs {
                                run_config
                                    .permissions
                                    .additional_read_roots
                                    .push(inputs.root.clone());
                                let context = include_str!("../../assets/prompts/remote_inputs.md")
                                    .replace("{{inputs}}", &inputs.context());
                                run_config.model.system_prompt = format!(
                                    "{}\n\n{context}",
                                    run_config.model.system_prompt.trim_end()
                                );
                                Arc::new(worker_run_service.with_remote_input_root(&inputs.root))
                            } else {
                                worker_run_service
                            };
                            let mut prompt = SharedConfirmationPrompt::new_with_root_control(
                                receiver_prompt,
                                worker_control.clone(),
                            );
                            let run = RunRequest {
                                prompt: request.prompt,
                                session_id: Some(session_id),
                                continue_last: false,
                                title: None,
                                cwd: worker_scope.app.workspace.cwd.clone(),
                                config: RunConfigInput::Resolved(run_config),
                                output_mode: OutputMode::Json,
                                show_reasoning_summary: false,
                                prompt_dispatch: None,
                                editor_context: None,
                                review_request: None,
                                image_paths: vec![],
                                run_control: worker_control,
                                session_access_mode_adoption: None,
                                agent_confirmation: Some(prompt.clone()),
                                agent_context: None,
                                admission_kind: RunAdmissionKind::RemoteTask { job_id: id },
                                expected_active_turn: ActiveTurnExpectation::Idle {
                                    latest_turn_id: None,
                                    revision: 0,
                                },
                            };
                            let _ = worker_run_service
                                .execute(AppCommand::Run(run), &mut RemoteRenderer, &mut prompt)
                                .await;
                            // Freeze only canonical changes for this exact admitted turn.
                            // A failed capture remains unavailable; it is never reported as
                            // an empty successful artifact or retried against a later workspace.
                            if let Ok(Some(current)) = worker_scope
                                .app
                                .store
                                .remote_job_store()
                                .get(&worker_job.principal_id, worker_job.id)
                            {
                                let _ = super::artifacts::capture_outputs(
                                    &worker_scope.app.store,
                                    &current,
                                    &worker_scope.app.workspace,
                                );
                            }
                            drop(inputs);
                            if let Some(route) = route {
                                route.finish().await;
                            }
                            if let (Some(network), Some(authority)) =
                                (&worker_network, &worker_authority)
                            {
                                let _ = worker_service
                                    .reconcile_network_receipt(
                                        network,
                                        worker_job,
                                        &authority.grant_id,
                                    )
                                    .await;
                            }
                        })
                        .map_err(|_| PublishCallError::Unavailable)?;
                    state.workers.insert(
                        job.session_id,
                        JobWorker {
                            profile: scope.profile.id,
                            run_control: control,
                            run_service: run_service.clone(),
                            managed_shell_lifetime,
                            handle,
                        },
                    );
                    let _ = start_tx.send(());
                }
                job
            }
        };
        self.row(job).await
    }

    pub(super) async fn row(&self, job: StoredRemoteJob) -> Result<RemoteJobRow, PublishCallError> {
        // Re-read the mapping after worker start: its admitted turn commits with history.
        let store = self.inner.process.store();
        let job = store
            .remote_job_store()
            .get(&job.principal_id, job.id)
            .map_err(|_| PublishCallError::Unavailable)?
            .ok_or(PublishCallError::InvalidTarget)?;
        let session = store
            .session_repo()
            .get_session(job.session_id)
            .await
            .map_err(|_| PublishCallError::InvalidTarget)?;
        let terminal = match job.admitted_turn_id {
            Some(turn) => store
                .session_repo()
                .durable_terminal_for_turn(job.session_id, turn)
                .await
                .map_err(|_| PublishCallError::Unavailable)?,
            None => None,
        };
        let (active, cancelling) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| PublishCallError::Unavailable)?;
            state.reap();
            let worker = state.workers.get(&job.session_id);
            (
                worker.is_some(),
                worker.is_some_and(|worker| {
                    worker.run_control.is_cancelled()
                        || state
                            .profiles
                            .get(&worker.profile)
                            .is_some_and(|scope| scope.admission_closed.is_cancelled())
                }),
            )
        };
        let mut result = None;
        let state = if active && cancelling {
            RemoteJobState::Cancelling
        } else if let Some(terminal) = &terminal {
            match terminal.session_status() {
                SessionStatus::Completed => RemoteJobState::Completed,
                SessionStatus::Cancelled => RemoteJobState::Interrupted,
                _ => RemoteJobState::Failed,
            }
        } else if active && self.inner.approvals.waiting(job.id) {
            RemoteJobState::AwaitingApproval
        } else if active && job.admitted_turn_id.is_some() {
            RemoteJobState::Running
        } else if active {
            RemoteJobState::Accepted
        } else {
            RemoteJobState::Interrupted
        };
        let terminal_committed = terminal.is_some();
        if let Some(terminal) = terminal {
            if let Some(response) = terminal.final_response_id {
                let content = store
                    .protocol_event_store()
                    .assistant_content_for_response(job.session_id, response)
                    .map_err(|_| PublishCallError::Unavailable)?;
                result = content.map(|parts| {
                    parts
                        .into_iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            }
            if result.is_none() && state == RemoteJobState::Failed {
                result = Some(
                    "受入側の実行が失敗しました。受入端末の作業履歴を確認してください。".into(),
                );
            }
        }
        let result_truncated = result
            .as_ref()
            .is_some_and(|text| text.len() > MAX_RESULT_BYTES);
        if let Some(text) = &mut result {
            let mut limit = MAX_RESULT_BYTES.min(text.len());
            while !text.is_char_boundary(limit) {
                limit -= 1;
            }
            text.truncate(limit);
        }
        let network = serde_json::from_str::<Value>(&job.scope_json)
            .ok()
            .and_then(|value| value.get("network").cloned())
            .and_then(|value| {
                serde_json::from_value::<crate::device_network::GrantClaims>(value).ok()
            })
            .map(|claims| RemoteJobNetwork {
                origin_device_id: claims.origin_device_id,
                actor_device_id: claims.actor_device_id,
                root_task_id: claims.root_task_id,
                device_path: claims.device_path,
            });
        Ok(RemoteJobRow {
            job_id: job.id,
            profile_id: PublishProfileId(job.profile_id),
            parent: job.parent,
            prompt_preview: job.prompt_preview,
            session_id: job.session_id,
            state,
            model: session.model,
            result,
            result_truncated,
            can_stop: active && !cancelling && !terminal_committed,
            network,
        })
    }
}

struct RemoteDispatcher {
    service: RemoteJobService,
    scope: Arc<ProfileScope>,
}

impl RemoteDispatcher {
    async fn artifacts(
        &self,
        arguments: Value,
        authority: Option<crate::device_network::VerifiedGrant>,
    ) -> Result<Value, PublishCallError> {
        let request: ArtifactsRequest =
            serde_json::from_value(arguments).map_err(|_| PublishCallError::InvalidArguments)?;
        let scope = self.scope.clone();
        let value = self
            .service
            .submit(move |service| async move {
                let principal = authority
                    .as_ref()
                    .map(network_principal)
                    .unwrap_or_else(|| scope.profile.id.0.to_string());
                let store = service.inner.process.store().remote_job_store();
                let job = store
                    .get(&principal, request.job_id)
                    .map_err(|_| PublishCallError::Unavailable)?
                    .ok_or(PublishCallError::InvalidTarget)?;
                if job.profile_id != scope.profile.id.0
                    || authority
                        .as_ref()
                        .is_some_and(|authority| !authorized_job(&job, authority))
                {
                    return Err(PublishCallError::InvalidTarget);
                }
                let bundle = store
                    .artifact_bundle(job.id, request.version.as_deref())
                    .map_err(|_| PublishCallError::Unavailable)?
                    .ok_or(PublishCallError::Unavailable)?;
                serde_json::to_value(bundle).map_err(|_| PublishCallError::Unavailable)
            })
            .await?;
        Ok(
            json!({"content":[{"type":"text","text":"The verified file bundle is in structuredContent."}],"structuredContent":value,"isError":false}),
        )
    }
}

fn network_principal(authority: &crate::device_network::VerifiedGrant) -> String {
    use sha2::{Digest, Sha256};
    let claims = authority.claims();
    let bytes = serde_json::to_vec(&(
        &claims.hub_id,
        &claims.origin_device_id,
        &claims.actor_device_id,
        &claims.profile_id,
    ))
    .expect("string identity");
    format!("hub-{:x}", Sha256::digest(bytes))
}

fn authorized_job(job: &StoredRemoteJob, authority: &crate::device_network::VerifiedGrant) -> bool {
    let Some(stored) = serde_json::from_str::<Value>(&job.scope_json)
        .ok()
        .and_then(|value| value.get("network").cloned())
    else {
        return false;
    };
    serde_json::from_value::<crate::device_network::GrantClaims>(stored)
        .is_ok_and(|claims| claims == *authority.claims())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    job_id: Option<Ulid>,
    request_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRequest {
    job_id: Ulid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactsRequest {
    job_id: Ulid,
    version: Option<String>,
}

#[async_trait]
impl PublishToolDispatcher for RemoteDispatcher {
    fn tool_descriptors(&self) -> Vec<Value> {
        let mut descriptors = vec![
            json!({"name":"delegate_task","description":"Delegate a natural-language task to this receiver's configured project or temp workspace. Returns a durable job promptly. Use the same request_key after uncertain delivery; never automatically create a replacement job.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"request_key":{"type":"string","maxLength":128},"parent":{"type":"object","additionalProperties":false,"properties":{"peer_id":{"type":"string"},"task_id":{"type":"string"},"turn_id":{"type":"string"}},"required":["peer_id","task_id","turn_id"]},"prompt":{"type":"string","maxLength":32768}},"required":["request_key","parent","prompt"]}}),
            json!({"name":"task_status","description":"Read this principal's accepted remote job. Supply exactly one of job_id or request_key. A disconnected caller does not imply a stopped job.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"job_id":{"type":"string"},"request_key":{"type":"string"}},"oneOf":[{"required":["job_id"]},{"required":["request_key"]}]},"annotations":{"readOnlyHint":true}}),
            json!({"name":"cancel_task","description":"Request cancellation of one accepted job; stopping is confirmed only when its real execution settles.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"job_id":{"type":"string"}},"required":["job_id"]}}),
            json!({"name":"task_artifacts","description":"Read the frozen file versions produced by one settled job. No files are installed or overwritten by this read. Unavailable means no verified snapshot exists.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"job_id":{"type":"string"},"version":{"type":"string","pattern":"^[0-9a-f]{64}$"}},"required":["job_id"]},"annotations":{"readOnlyHint":true}}),
        ];
        descriptors[0]["inputSchema"]["properties"]["inputs"] = json!({"type":"array","maxItems":8,"description":"Optional UTF-8 input file versions, at most 64 KiB per file and 256 KiB total. They remain read-only reference copies, separate from the receiver workspace.","items":{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"Portable relative file path"},"sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"text":{"type":"string"}},"required":["path","sha256","text"]}});
        descriptors
    }

    async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value, PublishCallError> {
        if self.scope.network.is_some() {
            return Err(PublishCallError::Unavailable);
        }
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        if self.scope.admission_closed.is_cancelled() {
            return Err(PublishCallError::Unavailable);
        }
        if name == "task_artifacts" {
            return self.artifacts(arguments, None).await;
        }
        let scope = self.scope.clone();
        let name = name.to_string();
        let row = self
            .service
            .submit(move |service| async move {
                match name.as_str() {
                    "delegate_task" => {
                        service
                            .start_job(
                                &scope,
                                serde_json::from_value(arguments)
                                    .map_err(|_| PublishCallError::InvalidArguments)?,
                                None,
                                cancel.clone(),
                            )
                            .await
                    }
                    "task_status" => {
                        let request: StatusRequest = serde_json::from_value(arguments)
                            .map_err(|_| PublishCallError::InvalidArguments)?;
                        let store = service.inner.process.store().remote_job_store();
                        let principal = scope.profile.id.0.to_string();
                        let job = match (request.job_id, request.request_key) {
                            (Some(id), None) => store.get(&principal, id),
                            (None, Some(key)) if !key.is_empty() && key.len() <= 128 => {
                                store.find_request(&principal, &key)
                            }
                            _ => return Err(PublishCallError::InvalidArguments),
                        }
                        .map_err(|_| PublishCallError::Unavailable)?
                        .ok_or(PublishCallError::InvalidTarget)?;
                        service.row(job).await
                    }
                    "cancel_task" => {
                        let request: CancelRequest = serde_json::from_value(arguments)
                            .map_err(|_| PublishCallError::InvalidArguments)?;
                        service
                            .cancel_job_inner(scope.profile.id, request.job_id)
                            .await
                    }
                    _ => Err(PublishCallError::ToolUnavailable),
                }
            })
            .await?;
        // Cancelling this MCP response does not cancel or replay an already accepted job.
        let value = serde_json::to_value(row).map_err(|_| PublishCallError::Unavailable)?;
        Ok(
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false}),
        )
    }

    async fn call_authorized(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
        authority: crate::device_network::VerifiedGrant,
    ) -> Result<Value, PublishCallError> {
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        if self.scope.network.is_none()
            || authority.claims().profile_id != self.scope.profile.id.0.to_string()
            || authority.claims().mode != "agent"
        {
            return Err(PublishCallError::Unavailable);
        }
        if name == "task_artifacts" {
            return self.artifacts(arguments, Some(authority)).await;
        }
        let scope = self.scope.clone();
        let name = name.to_owned();
        let row = self
            .service
            .submit(move |service| async move {
                let principal = network_principal(&authority);
                if name == "delegate_task" {
                    if authority.claims().scope_id != service.network_scope_id(scope.profile.id)? {
                        return Err(PublishCallError::InvalidTarget);
                    }
                    let mut request: RemoteTaskRequest = serde_json::from_value(arguments)
                        .map_err(|_| PublishCallError::InvalidArguments)?;
                    if request.request_key != authority.claims().request_key {
                        return Err(PublishCallError::InvalidArguments);
                    }
                    // Display ancestry is derived from authenticated claims; caller text has no identity authority.
                    request.parent = RemoteJobParent {
                        peer_id: authority.claims().actor_device_id.clone(),
                        task_id: authority.claims().root_task_id.clone(),
                        turn_id: authority
                            .claims()
                            .parent_job_id
                            .clone()
                            .unwrap_or_else(|| authority.claims().root_task_id.clone()),
                    };
                    service
                        .start_job(&scope, request, Some(authority), cancel)
                        .await
                } else {
                    let store = service.inner.process.store().remote_job_store();
                    let job = match name.as_str() {
                        "task_status" => {
                            let request: StatusRequest = serde_json::from_value(arguments)
                                .map_err(|_| PublishCallError::InvalidArguments)?;
                            match (request.job_id, request.request_key) {
                                (Some(id), None) => store.get(&principal, id),
                                (None, Some(key)) if key == authority.claims().request_key => {
                                    store.find_request(&principal, &key)
                                }
                                _ => return Err(PublishCallError::InvalidArguments),
                            }
                        }
                        "cancel_task" => {
                            let request: CancelRequest = serde_json::from_value(arguments)
                                .map_err(|_| PublishCallError::InvalidArguments)?;
                            store.get(&principal, request.job_id)
                        }
                        _ => return Err(PublishCallError::ToolUnavailable),
                    }
                    .map_err(|_| PublishCallError::Unavailable)?
                    .ok_or(PublishCallError::InvalidTarget)?;
                    if !authorized_job(&job, &authority) {
                        return Err(PublishCallError::InvalidTarget);
                    }
                    if name == "cancel_task" {
                        service.cancel_job_inner(scope.profile.id, job.id).await
                    } else {
                        service.row(job).await
                    }
                }
            })
            .await?;
        let value = serde_json::to_value(row).map_err(|_| PublishCallError::Unavailable)?;
        Ok(
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false}),
        )
    }
}

struct RemoteRenderer;
impl EventRenderer for RemoteRenderer {
    fn render(&mut self, _: &crate::session::RunEvent) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn finish(&mut self, _: &crate::session::RunSummary) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_list(
        &mut self,
        _: &[crate::session::SessionRecord],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_loaded_sessions(
        &mut self,
        _: &crate::session::LoadedSessionList,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_items(
        &mut self,
        _: &crate::session::SessionRecord,
        _: &[HistoryItem],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_page(
        &mut self,
        _: &crate::session::CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_read(
        &mut self,
        _: &crate::session::CanonicalSessionRead,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_rejoin(
        &mut self,
        _: &crate::session::RunningSessionRejoin,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_turn_page(
        &mut self,
        _: &crate::session::CanonicalTurnPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_runtime_event_page(
        &mut self,
        _: &crate::session::CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
