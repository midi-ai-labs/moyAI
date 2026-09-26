//! App-owned, finite command lifetimes. Shell/sandbox remain the process-tree owners.
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use super::{CommandOutput, ShellInput, execute_shell_command_observed, shell_permission_intent};
use crate::error::ToolError;
use crate::session::SessionId;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::sandbox_process::ProcessStarted;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard};

const MAX_ACTIVE: usize = 8;
const MAX_RECORDS: usize = 64;
const MAX_CAPTURE: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    workspace: String,
    authority: Authority,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Authority {
    Local(SessionId),
    Remote {
        profile: Ulid,
        principal: String,
        origin: Option<String>,
        task: String,
    },
}

impl Owner {
    fn from_context(ctx: &ToolContext<'_>) -> Result<Self, ToolError> {
        let root = ctx
            .agent
            .map_or(ctx.session.session.id, |agent| agent.root_session_id());
        let job = ctx
            .services
            .store
            .remote_job_store()
            .job_for_session(root)?;
        let authority = if let Some(job) = job {
            let scope: serde_json::Value = serde_json::from_str(&job.scope_json)?;
            let network = scope
                .get("network")
                .map(|value| {
                    serde_json::from_value::<crate::device_network::GrantClaims>(value.clone())
                })
                .transpose()?;
            Authority::Remote {
                profile: job.profile_id,
                principal: job.principal_id,
                origin: network
                    .as_ref()
                    .map(|claims| claims.origin_device_id.clone()),
                task: network.map_or(job.parent.task_id, |claims| claims.root_task_id),
            }
        } else {
            Authority::Local(root)
        };
        Ok(Self {
            workspace: PathGuard::stable_identity_key(ctx.workspace.authority_root()),
            authority,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Starting,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl State {
    fn terminal(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize)]
struct Snapshot {
    process_id: Ulid,
    state: State,
    command: String,
    workdir: camino::Utf8PathBuf,
    timeout_ms: u64,
    sandbox: serde_json::Value,
    pid: Option<u32>,
    started_at: Option<String>,
    finished_at: Option<String>,
    elapsed_ms: Option<u64>,
    /// Successful process exit does not attest to HTTP readiness or test assertions.
    readiness: &'static str,
    success: Option<bool>,
    result: Option<CommandOutput>,
    error: Option<String>,
}

struct Record {
    owner: Owner,
    receiver_scope_id: Option<Ulid>,
    cancel: CancellationToken,
    snapshot: watch::Receiver<Snapshot>,
    worker: std::thread::JoinHandle<()>,
    retain_for_delegation: bool,
    retain_after_turn: bool,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RetainedService {
    pub service_id: Ulid,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub retain_after_turn: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedServiceState {
    Running,
    Stopping,
    Stopped,
    Unknown,
}

#[derive(Default)]
struct Registry {
    closing: bool,
    records: HashMap<Ulid, Record>,
}

impl Drop for Registry {
    fn drop(&mut self) {
        // Workers never hold this registry. Cancel in parallel before joining outside any lock.
        for record in self.records.values() {
            record.cancel.cancel();
        }
        for (_, record) in self.records.drain() {
            let _ = record.worker.join();
        }
    }
}

/// One instance per AppProcessRuntime, also shared by rebuilt workspaces and receivers.
#[derive(Clone, Default)]
pub struct ManagedShells {
    inner: Arc<Mutex<Registry>>,
    receiver_lifetime: Option<CancellationToken>,
    receiver_scope_id: Option<Ulid>,
}

impl ManagedShells {
    pub(crate) fn with_lifetime(&self, lifetime: CancellationToken, scope_id: Ulid) -> Self {
        Self {
            inner: self.inner.clone(),
            receiver_lifetime: Some(lifetime),
            receiver_scope_id: Some(scope_id),
        }
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Registry>, ToolError> {
        self.inner
            .lock()
            .map_err(|_| ToolError::Message("managed shell registry unavailable".into()))
    }

    #[cfg(test)]
    fn start<F, Fut>(
        &self,
        owner: Owner,
        command: String,
        workdir: camino::Utf8PathBuf,
        timeout_ms: u64,
        sandbox: serde_json::Value,
        parent_cancel: CancellationToken,
        execute: F,
    ) -> Result<(watch::Receiver<Snapshot>, CancellationToken), ToolError>
    where
        F: FnOnce(CancellationToken, ProcessStarted) -> Fut + Send + 'static,
        Fut: Future<Output = Result<CommandOutput, ToolError>> + 'static,
    {
        self.start_with_retention(
            owner,
            command,
            workdir,
            timeout_ms,
            sandbox,
            parent_cancel,
            false,
            false,
            execute,
        )
    }

    fn start_with_retention<F, Fut>(
        &self,
        owner: Owner,
        command: String,
        workdir: camino::Utf8PathBuf,
        timeout_ms: u64,
        sandbox: serde_json::Value,
        parent_cancel: CancellationToken,
        retain_for_delegation: bool,
        retain_after_turn: bool,
        execute: F,
    ) -> Result<(watch::Receiver<Snapshot>, CancellationToken), ToolError>
    where
        F: FnOnce(CancellationToken, ProcessStarted) -> Fut + Send + 'static,
        Fut: Future<Output = Result<CommandOutput, ToolError>> + 'static,
    {
        let mut registry = self.lock()?;
        if registry.closing {
            return Err(ToolError::Message(
                "managed shells are shutting down".into(),
            ));
        }
        if self
            .receiver_lifetime
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolError::Message(
                "the receiver task authority was revoked".into(),
            ));
        }
        if registry
            .records
            .values()
            .filter(|record| !record.worker.is_finished())
            .count()
            >= MAX_ACTIVE
        {
            return Err(ToolError::Message(format!(
                "at most {MAX_ACTIVE} managed commands may be active"
            )));
        }
        if registry.records.len() >= MAX_RECORDS {
            let oldest = registry
                .records
                .iter()
                .filter(|(_, r)| r.worker.is_finished())
                .map(|(id, _)| *id)
                .min();
            if let Some(id) = oldest {
                let record = registry.records.remove(&id).expect("selected record");
                let _ = record.worker.join();
            } else {
                return Err(ToolError::Message(
                    "managed command history capacity reached".into(),
                ));
            }
        }
        let expires_at_ms =
            (chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_add(timeout_ms);
        let id = Ulid::new();
        let (sender, receiver) = watch::channel(Snapshot {
            process_id: id,
            state: State::Starting,
            command,
            workdir,
            timeout_ms,
            sandbox,
            pid: None,
            started_at: None,
            finished_at: None,
            elapsed_ms: None,
            readiness: "not_checked",
            success: None,
            result: None,
            error: None,
        });
        let cancel = parent_cancel.child_token();
        let worker_cancel = cancel.clone();
        let lifetime = self.receiver_lifetime.clone();
        let worker = std::thread::Builder::new()
            .name("moyai-managed-shell".into())
            .spawn(move || {
                let clock = Instant::now();
                let start_sender = sender.clone();
                let started: ProcessStarted = Arc::new(move |pid| {
                    start_sender.send_modify(|value| {
                        value.state = State::Running;
                        value.pid = Some(pid);
                        value.started_at = Some(chrono::Utc::now().to_rfc3339());
                    });
                });
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async move {
                        if lifetime
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        {
                            worker_cancel.cancel();
                        }
                        let mut work = Box::pin(execute(worker_cancel.clone(), started));
                        tokio::select! {
                            biased;
                            _ = async {
                                match lifetime {
                                    Some(lifetime) => lifetime.cancelled().await,
                                    None => std::future::pending::<()>().await,
                                }
                            } => {
                                worker_cancel.cancel();
                                // Keep the execution future alive through its owned cleanup.
                                work.await
                            }
                            result = &mut work => result,
                        }
                    })
                }))
                .unwrap_or_else(|_| {
                    Err(ToolError::Message("managed command worker panicked".into()))
                });
                sender.send_modify(|value| {
                    value.finished_at = Some(chrono::Utc::now().to_rfc3339());
                    value.elapsed_ms =
                        Some(clock.elapsed().as_millis().min(u64::MAX as u128) as u64);
                    match result {
                        Ok(output) => {
                            let success = output.exit_code == Some(0)
                                && !output.timed_out
                                && !output.cancelled
                                && !output.cleanup_failed;
                            value.state = if output.cancelled {
                                State::Cancelled
                            } else if output.timed_out {
                                State::TimedOut
                            } else if success {
                                State::Completed
                            } else {
                                State::Failed
                            };
                            value.success = Some(success);
                            value.result = Some(output);
                        }
                        Err(error) => {
                            value.state = State::Failed;
                            value.success = Some(false);
                            value.error = Some(crate::tool::truncate::clip_text_to_char_boundary(
                                &error.to_string(),
                                4096,
                            ));
                        }
                    }
                });
            })?;
        registry.records.insert(
            id,
            Record {
                owner,
                receiver_scope_id: self.receiver_scope_id,
                cancel: cancel.clone(),
                snapshot: receiver.clone(),
                worker,
                retain_for_delegation,
                retain_after_turn,
                expires_at_ms,
            },
        );
        Ok((receiver, cancel))
    }

    fn lookup(
        &self,
        owner: &Owner,
        id: Ulid,
    ) -> Result<(watch::Receiver<Snapshot>, CancellationToken), ToolError> {
        let registry = self.lock()?;
        let record = registry
            .records
            .get(&id)
            .filter(|record| &record.owner == owner)
            .ok_or_else(|| {
                ToolError::Message(
                    "managed command is unavailable in this task and workspace".into(),
                )
            })?;
        Ok((record.snapshot.clone(), record.cancel.clone()))
    }

    pub(crate) fn begin_shutdown(&self) {
        if let Ok(mut registry) = self.inner.lock() {
            registry.closing = true;
            for record in registry.records.values() {
                record.cancel.cancel();
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.begin_shutdown();
        loop {
            let finished = self
                .inner
                .lock()
                .is_ok_and(|registry| registry.records.values().all(|r| r.worker.is_finished()));
            if finished {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let records = self
            .inner
            .lock()
            .map(|mut r| std::mem::take(&mut r.records))
            .unwrap_or_default();
        for (_, record) in records {
            let _ = record.worker.join();
        }
    }

    pub(crate) fn cancel_profile(&self, profile: Ulid) {
        self.cancel_matching(|owner| matches!(&owner.authority, Authority::Remote { profile: p, .. } if *p == profile));
    }

    pub(crate) fn cancel_local(&self, root: SessionId) {
        self.cancel_matching(
            |owner| matches!(&owner.authority, Authority::Local(session) if *session == root),
        );
    }

    pub(crate) fn cancel_lineages(&self, lineages: &[(String, String)]) {
        self.cancel_matching(|owner| {
            matches!(&owner.authority,
            Authority::Remote { origin: Some(origin), task, .. }
            if lineages.iter().any(|(o, t)| o == origin && t == task))
        });
    }

    fn cancel_matching(&self, predicate: impl Fn(&Owner) -> bool) {
        if let Ok(registry) = self.inner.lock() {
            for record in registry.records.values().filter(|r| predicate(&r.owner)) {
                record.cancel.cancel();
            }
        }
    }

    pub(crate) fn has_profile_work(&self, profile: Ulid) -> bool {
        self.inner.lock().map_or(true, |registry| registry.records.values().any(|record|
            !record.worker.is_finished() && matches!(&record.owner.authority, Authority::Remote { profile: p, .. } if *p == profile)))
    }

    /// A terminal local turn may still own a managed process. The Runner keeps its execution
    /// capacity occupied until the existing process-tree owner confirms that worker has ended.
    pub(crate) fn has_local_work(&self, root: SessionId) -> bool {
        self.has_local_work_for(root, self.receiver_scope_id)
    }

    pub(crate) fn has_local_work_in_scope(&self, root: SessionId, scope_id: Ulid) -> bool {
        self.has_local_work_for(root, Some(scope_id))
    }

    fn has_local_work_for(&self, root: SessionId, scope_id: Option<Ulid>) -> bool {
        self.inner.lock().map_or(true, |registry| {
            registry.records.values().any(|record| {
                !record.worker.is_finished()
                    && record.receiver_scope_id == scope_id
                    && matches!(&record.owner.authority, Authority::Local(session) if *session == root)
            })
        })
    }

    /// Only one explicitly marked, running service can remain when a shared turn
    /// delegates. Ordinary commands retain the existing quiescent-yield rule.
    pub(crate) fn delegable_service(&self, root: SessionId) -> Option<RetainedService> {
        self.delegable_service_for(root, self.receiver_scope_id)
    }

    fn delegable_service_for(
        &self,
        root: SessionId,
        scope_id: Option<Ulid>,
    ) -> Option<RetainedService> {
        let registry = self.inner.lock().ok()?;
        let mut active = registry.records.iter().filter(|(_, record)| {
            !record.worker.is_finished()
                && record.receiver_scope_id == scope_id
                && matches!(&record.owner.authority, Authority::Local(session) if *session == root)
        });
        let (id, record) = active.next()?;
        if active.next().is_some()
            || !(record.retain_for_delegation || record.retain_after_turn)
            || record.cancel.is_cancelled()
            || record.snapshot.borrow().state != State::Running
            || record.snapshot.borrow().pid.is_none()
            || record.expires_at_ms <= chrono::Utc::now().timestamp_millis().max(0) as u64
        {
            return None;
        }
        Some(RetainedService {
            service_id: *id,
            expires_at_ms: record.expires_at_ms,
            retain_after_turn: record.retain_after_turn,
        })
    }

    pub(crate) fn completion_service_in_scope(
        &self,
        root: SessionId,
        scope_id: Ulid,
    ) -> Option<RetainedService> {
        self.delegable_service_for(root, Some(scope_id))
            .filter(|service| service.retain_after_turn)
    }

    pub(crate) fn retained_service_live(&self, service: RetainedService) -> bool {
        self.retained_service_state(service) == RetainedServiceState::Running
    }

    pub(crate) fn retained_service_state(&self, service: RetainedService) -> RetainedServiceState {
        self.inner
            .lock()
            .map_or(RetainedServiceState::Unknown, |registry| {
                let Some(record) = registry.records.get(&service.service_id).filter(|record| {
                    (record.retain_for_delegation || record.retain_after_turn)
                        && record.retain_after_turn == service.retain_after_turn
                        && record.expires_at_ms == service.expires_at_ms
                }) else {
                    return RetainedServiceState::Unknown;
                };
                classify_retained_state(
                    record.worker.is_finished(),
                    record.cancel.is_cancelled(),
                    &record.snapshot.borrow(),
                )
            })
    }

    pub(crate) fn cancel_retained_service(&self, id: Ulid) -> bool {
        self.inner.lock().is_ok_and(|registry| {
            registry.records.get(&id).is_some_and(|record| {
                if !(record.retain_for_delegation || record.retain_after_turn) {
                    return false;
                }
                record.cancel.cancel();
                true
            })
        })
    }
}

fn classify_retained_state(
    worker_finished: bool,
    cancelled: bool,
    snapshot: &Snapshot,
) -> RetainedServiceState {
    if worker_finished || snapshot.state.terminal() {
        RetainedServiceState::Stopped
    } else if cancelled {
        RetainedServiceState::Stopping
    } else if snapshot.state == State::Running && snapshot.pid.is_some() {
        RetainedServiceState::Running
    } else {
        RetainedServiceState::Unknown
    }
}

struct StartHandoff {
    cancel: CancellationToken,
    committed: bool,
}
impl Drop for StartHandoff {
    fn drop(&mut self) {
        if !self.committed {
            self.cancel.cancel();
        }
    }
}

pub struct ShellStartTool;
pub struct ShellStatusTool;
pub struct ShellStopTool;

#[async_trait(?Send)]
impl Tool for ShellStartTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = super::ShellTool.spec();
        spec.name = ToolName::ShellStart;
        spec.description = include_str!("../../../assets/prompts/shell_start.md");
        spec.input_schema["properties"]["timeout_ms"]["description"] = json!(
            "Execution lifetime limit in milliseconds. Normally omit to inherit model.request_timeout_ms from the executing PC. A shorter positive value is allowed; values above that setting are rejected. A running command is stopped at this limit."
        );
        spec.input_schema["properties"]["retain_for_delegation"] = json!({
            "type":"boolean",
            "default":false,
            "description":"This job hosts a server while it calls shared_delegate to another PC. With this option alone, the server stops when this job finishes. If this job will return before its caller uses the server, use retain_after_turn instead."
        });
        spec.input_schema["properties"]["retain_after_turn"] = json!({
            "type":"boolean",
            "default":false,
            "description":"Keep this server running after this shared job returns, for the requested follow-up use or testing by its caller, or a requested preview. Only one server can be retained; it remains visible for explicit stop and ends at timeout_ms."
        });
        spec
    }

    async fn execute(
        &self,
        raw: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let retain_for_delegation = match raw.get("retain_for_delegation") {
            None | Some(serde_json::Value::Bool(false)) => false,
            Some(serde_json::Value::Bool(true)) => true,
            Some(_) => {
                return Err(ToolError::Message(
                    "retain_for_delegation must be a boolean".into(),
                ));
            }
        };
        let retain_after_turn = match raw.get("retain_after_turn") {
            None | Some(serde_json::Value::Bool(false)) => false,
            Some(serde_json::Value::Bool(true)) => true,
            Some(_) => {
                return Err(ToolError::Message(
                    "retain_after_turn must be a boolean".into(),
                ));
            }
        };
        let input: ShellInput = serde_json::from_value(raw)?;
        let owner = Owner::from_context(&ctx)?;
        let max_timeout_ms = ctx.config.model.request_timeout_ms;
        let timeout_ms = input.timeout_ms.unwrap_or(max_timeout_ms);
        if timeout_ms == 0 || timeout_ms > max_timeout_ms {
            return Err(ToolError::Message(format!(
                "timeout_ms must be between 1 and {max_timeout_ms} (model.request_timeout_ms)"
            )));
        }
        let mut intent = shell_permission_intent(ctx.workspace, ctx.config, &input)?;
        let lifetime = if timeout_ms.is_multiple_of(60_000) {
            format!("{}分", timeout_ms / 60_000)
        } else if timeout_ms.is_multiple_of(1_000) {
            format!("{}秒", timeout_ms / 1_000)
        } else {
            format!("{}秒", timeout_ms as f64 / 1_000.0)
        };
        intent.details.push(format!(
            "実行時間の上限: 起動から{lifetime}。上限に達すると自動で停止します。必要に応じて、AIに停止を依頼することもできます。"
        ));
        let admission = ctx
            .confirm_if_needed_with_details(
                AccessKind::Shell,
                intent.description.clone(),
                intent.details,
                intent.targets,
                intent.outside_workspace,
                intent.risks,
            )
            .await?;
        let fence = ctx.run_mutation_fence.clone();
        let shell = ctx.config.shell.clone();
        let max_output = ctx.config.tool_output.max_bytes.clamp(1, MAX_CAPTURE);
        let command = input.command;
        let workdir = intent.guarded.absolute.clone();
        let sandbox = serde_json::to_value(admission.sandbox_plan().audit_description())?;
        // Keep the startup ticket until handoff, independently of the process lifetime.
        let worker_admission = admission.clone();
        let (mut snapshot, cancel) = ctx.services.managed_shells.start_with_retention(
            owner,
            command.clone(),
            workdir.clone(),
            timeout_ms,
            sandbox,
            ctx.cancel.clone(),
            retain_for_delegation,
            retain_after_turn,
            move |cancel, started| async move {
                fence.assert_owned().await?;
                worker_admission.admit()?;
                PathGuard::revalidate(&intent.guarded)?;
                execute_shell_command_observed(
                    &shell,
                    &workdir,
                    &command,
                    timeout_ms,
                    max_output,
                    cancel,
                    worker_admission.sandbox_plan(),
                    intent.execution.family,
                    intent.execution.environment,
                    intent.execution.programs,
                    Some(started),
                )
                .await
            },
        )?;
        let mut handoff = StartHandoff {
            cancel,
            committed: false,
        };
        // Do not return a process ID as running before the actual owned process has spawned.
        while snapshot.borrow_and_update().state == State::Starting {
            snapshot
                .changed()
                .await
                .map_err(|_| ToolError::Message("managed command startup lost its owner".into()))?;
        }
        let snapshot = snapshot.borrow().clone();
        if snapshot.pid.is_some() {
            // A failed settlement leaves handoff uncommitted, so the owned process
            // is cancelled instead of returning a misleading successful startup.
            admission.finish_started_effect()?;
        }
        let result = snapshot_result(snapshot, false, &ctx)?;
        handoff.committed = true;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectInput {
    process_id: Ulid,
    #[serde(default)]
    wait_ms: u64,
}

fn inspection_spec(name: ToolName, stop: bool) -> ToolSpec {
    ToolSpec {
        name,
        effect: if stop {
            ToolEffectPolicy::destructive()
        } else {
            ToolEffectPolicy::read()
        },
        description: if stop {
            "Stop an owned managed command and its descendants. Use the opaque process_id returned by shell_start. Wait for cleanup; stopping is not a successful exit. No arbitrary PID is accepted."
        } else {
            "Read a managed command's observed lifecycle and bounded final output. Use only its returned process_id in the same task/workspace. Running is not HTTP readiness; success describes the command's exit only. Output is available after exit. Wait without another model request using wait_ms (up to 60000). IDs do not survive app restart."
        },
        input_schema: json!({"type":"object", "required":["process_id"], "additionalProperties":false,
            "properties":{"process_id":{"type":"string"},"wait_ms":{"type":"integer","minimum":0,"maximum":60000}}}),
    }
}

async fn inspect(
    raw: serde_json::Value,
    ctx: ToolContext<'_>,
    stop: bool,
) -> Result<ToolResult, ToolError> {
    let input: InspectInput = serde_json::from_value(raw)?;
    if input.wait_ms > 60_000 {
        return Err(ToolError::Message("wait_ms cannot exceed 60000".into()));
    }
    let owner = Owner::from_context(&ctx)?;
    let (mut receiver, cancel) = ctx
        .services
        .managed_shells
        .lookup(&owner, input.process_id)?;
    if stop {
        ctx.run_mutation_fence.assert_owned().await?;
        // This capability only cancels our already-owned process; it cannot start another effect.
        ctx.run_control
            .begin_tool_effect_admission()
            .ok_or(ToolError::RunInterrupted)?
            .admit()
            .map_err(|_| ToolError::RunInterrupted)?;
        cancel.cancel();
    }
    let wait_ms = if stop && input.wait_ms == 0 {
        12_000
    } else {
        input.wait_ms
    };
    let waiting = async {
        loop {
            if receiver.borrow_and_update().state.terminal() {
                break;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = tokio::time::timeout(Duration::from_millis(wait_ms), waiting) => {},
        _ = ctx.cancel.cancelled() => {},
    }
    let snapshot = receiver.borrow().clone();
    snapshot_result(snapshot, cancel.is_cancelled(), &ctx)
}

#[async_trait(?Send)]
impl Tool for ShellStatusTool {
    fn spec(&self) -> ToolSpec {
        inspection_spec(ToolName::ShellStatus, false)
    }
    async fn execute(
        &self,
        raw: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        inspect(raw, ctx, false).await
    }
}

#[async_trait(?Send)]
impl Tool for ShellStopTool {
    fn spec(&self) -> ToolSpec {
        inspection_spec(ToolName::ShellStop, true)
    }
    async fn execute(
        &self,
        raw: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        inspect(raw, ctx, true).await
    }
}

fn snapshot_result(
    snapshot: Snapshot,
    stopping: bool,
    ctx: &ToolContext<'_>,
) -> Result<ToolResult, ToolError> {
    let mut metadata = serde_json::to_value(&snapshot)?;
    metadata["output_available"] = json!(snapshot.result.is_some());
    if stopping && !snapshot.state.terminal() {
        metadata["state"] = json!("stopping");
    }
    let preview = ctx.services.truncator.preview(
        serde_json::to_string_pretty(&metadata)?,
        &ctx.config.tool_output,
        &ctx.services.storage_paths,
    )?;
    // Keep measured exit/cleanup facts even when the preview is truncated. Only text is bulky.
    if let Some(result) = metadata
        .get_mut("result")
        .and_then(serde_json::Value::as_object_mut)
    {
        result.remove("stdout");
        result.remove("stderr");
    }
    Ok(ToolResult {
        title: format!("Managed command {}", snapshot.process_id),
        output_text: preview.preview_text,
        metadata,
        truncated_output_path: preview.truncated_output_path,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: preview.internal_file_lease,
    })
}
