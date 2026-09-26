//! Shared-work sessions belong to the approved device and its current Hub binding.
//! The Hub authenticates the device certificate; the webview receives only a typed
//! projection, never the device key or its short-lived session token.
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{DeviceClient, DeviceNetworkService};
mod agent;
mod authentication;
mod collaboration;
mod files;
mod origin;
mod providing;
mod receipts;
mod runner_service;
pub use collaboration::{WorkHandover, WorkInbox};
pub use files::WorkAsset;
pub use origin::OriginWorkProjection;
use receipts::{Receipt, ReceiptOperation, ReceiptStore};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkPerson {
    pub user_id: String,
    pub display_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkPrincipal {
    pub user_id: String,
    pub display_name: String,
    pub administrator: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkProject {
    pub id: String,
    pub label: String,
    role: String,
    #[serde(default)]
    pub can_submit: bool,
    #[serde(default = "initial_participation_generation")]
    pub participation_generation: u64,
}
#[derive(Deserialize)]
struct CurrentDeviceProjects {
    projects: Vec<CurrentDeviceProject>,
}
#[derive(Deserialize)]
struct CurrentDeviceProject {
    id: String,
    can_control: bool,
    can_execute: bool,
    participation_generation: u64,
}
fn initial_participation_generation() -> u64 {
    1
}
impl WorkProject {
    pub(crate) fn allows_submission(&self) -> bool {
        matches!(self.role.as_str(), "contributor" | "manager")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkRunnerContact {
    pub last_contact_ms: Option<u64>,
    pub state: WorkRunnerContactState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRunnerContactState {
    Unconfirmed,
    Recent,
    Stale,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkSummary {
    pub id: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub origin_session_ref: Option<String>,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub state: String,
    pub environment_id: String,
    pub environment_label: String,
    #[serde(default)]
    pub device_label: Option<String>,
    pub requestor: WorkPerson,
    pub assignee: WorkPerson,
    pub wait_reason: Option<String>,
    pub uncertainty_reason: Option<String>,
    #[serde(default)]
    pub runner_contact: Option<WorkRunnerContact>,
    pub can_cancel: bool,
    #[serde(default)]
    pub can_continue: bool,
    #[serde(default)]
    pub can_revise: bool,
    #[serde(default)]
    pub revises_job_id: Option<String>,
    #[serde(default)]
    pub can_handover: bool,
    #[serde(default)]
    pub conversation_epoch: u64,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub start_before_ms: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkOccupant {
    pub job_id: String,
    pub title: String,
    pub project_id: String,
    pub project_label: String,
    pub user: WorkPerson,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkEnvironment {
    pub id: String,
    pub label: String,
    pub resource_id: String,
    pub runner_id: String,
    #[serde(default)]
    pub device_label: Option<String>,
    pub enabled: bool,
    pub capacity: u32,
    pub occupied: u32,
    pub occupants: Vec<WorkOccupant>,
    pub additional_visible_occupants: u32,
    pub other_occupants: u32,
    #[serde(default)]
    pub runner_contact: Option<WorkRunnerContact>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkExecutionDevice {
    pub device_id: String,
    pub device_label: Option<String>,
    pub environment_id: Option<String>,
    pub preparation_state: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkStatus {
    pub project_id: String,
    pub jobs: Vec<WorkSummary>,
    pub environments: Vec<WorkEnvironment>,
    #[serde(default)]
    pub execution_devices: Vec<WorkExecutionDevice>,
    pub next_before: Option<String>,
    pub next_environment_before: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkDetail {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub origin_session_ref: Option<String>,
    #[serde(default)]
    pub origin_turn_ref: Option<String>,
    #[serde(default)]
    pub origin_turn_epoch: Option<u64>,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub environment_id: String,
    pub title: String,
    pub input: Value,
    pub result: Option<Value>,
    pub state: String,
    #[serde(default)]
    pub wait_reason: Option<String>,
    #[serde(default)]
    pub uncertainty_reason: Option<String>,
    #[serde(default)]
    pub runner_contact: Option<WorkRunnerContact>,
    pub awaiting_child_id: Option<String>,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub start_before_ms: Option<u64>,
    #[serde(default)]
    pub continued_from_id: Option<String>,
    #[serde(default)]
    pub can_continue: bool,
    #[serde(default)]
    pub can_revise: bool,
    #[serde(default)]
    pub revises_job_id: Option<String>,
    #[serde(default)]
    pub can_handover: bool,
    #[serde(default)]
    pub conversation_epoch: u64,
    #[serde(default)]
    pub retained_services: Vec<WorkRetainedService>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkRetainedService {
    pub service_id: String,
    pub environment_id: String,
    pub expires_at_ms: u64,
    pub stop_requested: bool,
    pub uncertain: bool,
    #[serde(default)]
    pub can_stop: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkTranscript {
    pub items: Vec<WorkTranscriptItem>,
    pub next_after: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkTranscriptItem {
    pub position: u64,
    pub kind: String,
    pub payload: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkConversationHistory {
    pub project_id: String,
    pub conversation_id: String,
    pub snapshot: u64,
    #[serde(default)]
    pub conversation_epoch: u64,
    pub jobs: Vec<WorkConversationJob>,
    pub next_before: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkConversationJob {
    pub job: WorkSummary,
    pub input: Value,
    pub result: Option<Value>,
    pub artifacts: Vec<WorkAsset>,
    pub more_artifacts: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkConversation {
    pub id: String,
    pub title: String,
    pub latest_job_id: Option<String>,
    pub updated_at_ms: u64,
    pub revision: u64,
    #[serde(default)]
    pub delete_pending: bool,
    #[serde(default)]
    pub can_rename: bool,
    #[serde(default)]
    pub can_delete: bool,
    #[serde(default)]
    pub can_revise: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkConversationList {
    conversations: Vec<WorkConversation>,
    revision: String,
}
fn selected_child_matches_conversation(
    view: &SharedWorkProjection,
    project: &str,
    conversation: &WorkConversation,
) -> bool {
    view.detail.as_ref().is_some_and(|detail| {
        detail.parent_id.is_some()
            && !matches!(detail.state.as_str(), "succeeded" | "failed" | "cancelled")
            && view.selected_job_id.as_deref() == Some(detail.id.as_str())
            && detail.project_id == project
            && detail
                .conversation_id
                .as_deref()
                .unwrap_or(detail.root_id.as_str())
                == conversation.id.as_str()
            && conversation.latest_job_id.as_deref() == Some(detail.root_id.as_str())
    })
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct SharedWorkProjection {
    pub revision: String,
    pub generation: String,
    pub connected: bool,
    pub hub_url: String,
    pub enrollment: String,
    pub enrollment_error: Option<String>,
    pub principal: Option<WorkPrincipal>,
    pub expires_at_ms: Option<u64>,
    pub projects: Vec<WorkProject>,
    #[serde(default)]
    pub projects_stale: bool,
    pub project_access: Option<String>,
    pub selected_project_id: Option<String>,
    pub conversations: Vec<WorkConversation>,
    pub conversation_revision: String,
    pub selected_conversation_id: Option<String>,
    pub selected_job_id: Option<String>,
    pub leave_pending_project_id: Option<String>,
    pub status: Option<WorkStatus>,
    pub detail: Option<WorkDetail>,
    pub approval: Option<WorkApproval>,
    pub inputs: Vec<WorkAsset>,
    pub assets: Vec<WorkAsset>,
    pub transcript: Option<WorkTranscript>,
    pub conversation_history: Option<WorkConversationHistory>,
    pub handover: Option<WorkHandover>,
    pub inbox: Option<WorkInbox>,
    pub provider: Option<crate::runner::operations::RunnerOperationsProjection>,
    pub provider_draft: Option<crate::runner::provision::ProvisionTemplate>,
    pub provider_error: Option<String>,
    pub provider_scope: crate::runner::shared::ResourceScope,
    pub observed_at_ms: Option<u64>,
    pub error: Option<String>,
    pub feedback: Option<String>,
    pub submission_uncertain: bool,
    pub submission_storage_error: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SharedWorkCommand {
    OpenManagement,
    RetrySubmission,
    Refresh,
    Project {
        project_id: String,
    },
    NewConversation {
        project_id: String,
    },
    SelectConversation {
        project_id: String,
        conversation_id: String,
    },
    RenameConversation {
        project_id: String,
        conversation_id: String,
        title: String,
    },
    DeleteConversation {
        project_id: String,
        conversation_id: String,
    },
    LeaveProject {
        project_id: String,
    },
    BindProjectFolder {
        project_id: String,
        environment_id: String,
        directory: camino::Utf8PathBuf,
        access_mode: crate::config::AccessMode,
        expected_directory: Option<camino::Utf8PathBuf>,
    },
    Detail {
        project_id: String,
        job_id: String,
    },
    NextJobs {
        project_id: String,
    },
    NextEnvironments {
        project_id: String,
    },
    Latest {
        project_id: String,
    },
    Submit {
        project_id: String,
        title: String,
        prompt: String,
        #[serde(default)]
        start_before_ms: Option<u64>,
    },
    Continue {
        project_id: String,
        job_id: String,
        expected_revision: u64,
        prompt: String,
        #[serde(default)]
        start_before_ms: Option<u64>,
    },
    Revise {
        project_id: String,
        conversation_id: String,
        job_id: String,
        expected_revision: u64,
        prompt: String,
    },
    UploadInputs {
        project_id: String,
    },
    PrepareSample {
        project_id: String,
    },
    RemoveInput {
        project_id: String,
        asset_id: String,
    },
    SaveAsset {
        project_id: String,
        job_id: String,
        asset_id: String,
        import: bool,
    },
    TranscriptNext {
        project_id: String,
        job_id: String,
    },
    ConversationHistoryNext {
        project_id: String,
        conversation_id: String,
    },
    StopConversation {
        project_id: String,
        conversation_id: String,
    },
    Handover {
        project_id: String,
        job_id: String,
        expected_revision: u64,
        new_assignee_id: String,
    },
    InboxNext,
    InboxLatest,
    InboxOpen {
        notification_id: String,
    },
    ProviderStatus,
    ProviderStart,
    ProviderPrepare {
        id: String,
        label: String,
        access_mode: crate::config::AccessMode,
        allowed_child_environments: Vec<String>,
        #[serde(default)]
        resource_scope: crate::runner::shared::ResourceScope,
    },
    ProviderInstall {
        runner_id: ulid::Ulid,
    },
    ProviderRemoveTemplate {
        runner_id: ulid::Ulid,
        template_id: String,
    },
    ProviderOperation {
        runner_id: ulid::Ulid,
        operation: crate::runner::operations::RunnerOperation,
    },
    Cancel {
        project_id: String,
        job_id: String,
    },
    StopService {
        project_id: String,
        service_id: String,
    },
    Decide {
        project_id: String,
        job_id: String,
        approval_id: String,
        decision: WorkDecision,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkApproval {
    pub id: String,
    pub attempt_id: String,
    pub request: crate::tool::PermissionRequest,
    pub status: String,
    pub decision: Option<String>,
    pub expires_at_ms: u64,
    pub can_decide: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkDecision {
    Approve,
    Deny,
    Stop,
}
#[derive(Clone, Deserialize)]
struct LoginSession {
    token: String,
    principal: WorkPrincipal,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
struct Session {
    principal: WorkPrincipal,
    expires_at_ms: u64,
    #[serde(default)]
    project_access: Option<String>,
}
pub(super) struct SharedWorkOwner(Mutex<Runtime>, tokio::sync::Mutex<()>);
impl SharedWorkOwner {
    pub fn new(path: camino::Utf8PathBuf) -> Self {
        Self(
            Mutex::new(Runtime {
                receipts: ReceiptStore::new(path),
                ..Runtime::default()
            }),
            tokio::sync::Mutex::new(()),
        )
    }
    pub(super) fn reset_connection(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.clear();
            state.binding.clear();
            state.hub_binding.clear();
        }
    }
}
#[derive(Default)]
struct Runtime {
    generation: u64,
    revision: u64,
    query: u64,
    binding: String,
    hub_binding: String,
    token: Option<String>,
    view: SharedWorkProjection,
    before: Option<String>,
    environment_before: Option<String>,
    transcript_after: u64,
    history_before: Option<u64>,
    inbox_before: Option<String>,
    receipts: ReceiptStore,
    provider_binding: String,
}
impl Runtime {
    fn begin_command(&mut self, command: &SharedWorkCommand) -> (u64, u64, Option<String>) {
        self.query += 1;
        self.view.error = None;
        if !matches!(command, SharedWorkCommand::Refresh) {
            self.view.feedback = None;
        }
        let captured = (self.generation, self.query, self.token.clone());
        captured
    }
    fn clear(&mut self) {
        self.generation += 1;
        self.query += 1;
        self.token = None;
        self.view = SharedWorkProjection::default();
        self.before = None;
        self.environment_before = None;
        self.transcript_after = 0;
        self.history_before = None;
        self.inbox_before = None;
        self.provider_binding.clear();
    }
    fn expire_to_read_only_projects(&mut self) {
        let projects = self.view.projects.clone();
        let selected_project_id = self.view.selected_project_id.clone();
        self.clear();
        self.view.projects_stale = !projects.is_empty();
        self.view.selected_project_id = selected_project_id
            .filter(|selected| projects.iter().any(|project| &project.id == selected));
        self.view.projects = projects;
    }
    fn projection(&mut self, connected: bool, hub_url: String) -> SharedWorkProjection {
        self.revision += 1;
        self.view.revision = self.revision.to_string();
        self.view.generation = self.generation.to_string();
        self.view.connected = connected;
        self.view.hub_url = hub_url;
        self.view.submission_uncertain = self.view.principal.as_ref().is_some_and(|p| {
            self.receipts
                .pending(&self.hub_binding, &p.user_id)
                .is_some()
        });
        self.view.leave_pending_project_id = self.view.principal.as_ref().and_then(|principal| {
            self.receipts
                .pending_leave(&self.hub_binding, &principal.user_id)
                .and_then(|receipt| match &receipt.operation {
                    ReceiptOperation::LeaveProject { project_id } => Some(project_id.clone()),
                    _ => None,
                })
        });
        self.view.submission_storage_error = self
            .view
            .principal
            .as_ref()
            .and_then(|_| self.receipts.error().map(str::to_owned));
        self.view.clone()
    }
    fn require_current(&self, generation: u64, query: u64) -> Result<(), RequestError> {
        if self.generation != generation || self.query != query {
            return Err(RequestError::Local("利用者または表示対象が変わりました。"));
        }
        Ok(())
    }
    fn require_project(
        &self,
        project: &str,
        generation: u64,
        query: u64,
    ) -> Result<(), RequestError> {
        self.require_current(generation, query)?;
        if self.view.selected_project_id.as_deref() != Some(project) {
            return Err(RequestError::Local("表示中のプロジェクトが変わりました。"));
        }
        Ok(())
    }
    fn require_not_leaving(&self, project: &str) -> Result<(), RequestError> {
        if self.view.leave_pending_project_id.as_deref() == Some(project) {
            return Err(RequestError::Local(
                "このPCの離脱を確認中です。新しい依頼は送信できません。",
            ));
        }
        Ok(())
    }
}
struct Connection {
    client: DeviceClient,
    hub_id: String,
    binding: String,
    hub_binding: String,
}
#[derive(Debug)]
enum RequestError {
    Local(&'static str),
    Http(u16),
    Unavailable,
    Invalid,
    Filesystem(String),
}
impl RequestError {
    fn message(&self) -> &str {
        match self {
            Self::Local(message) => message,
            Self::Filesystem(message) => message,
            Self::Http(401) => {
                "このPCの利用資格を確認できません。Hubへの接続と、管理者による端末の承認を確認してください。"
            }
            Self::Http(403) => {
                "このPCに操作が許可されていません。Hub管理者に、端末の承認・利用者の関連付けとプロジェクトの操作PC設定を確認してもらってください。"
            }
            Self::Http(404) => "対象が見つかりません。最新の一覧を確認してください。",
            Self::Http(409) => "仕事の状態が変わりました。最新の状態を確認してください。",
            Self::Http(429) => "受付上限に達しています。しばらく待ってから再試行してください。",
            Self::Http(400) => "入力内容を確認してください。",
            Self::Unavailable => "Hubに接続できません。接続を確認して更新してください。",
            _ => "Hubの応答を確認できません。更新して再確認してください。",
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn submission_start_deadline(requested: Option<u64>, now: u64) -> Result<u64, RequestError> {
    let deadline = requested.unwrap_or_else(|| now.saturating_add(24 * 60 * 60 * 1000));
    if deadline <= now || deadline > i64::MAX as u64 {
        return Err(RequestError::Local(
            "開始期限には未来の日時を指定してください。空欄の場合は投入から24時間です。",
        ));
    }
    Ok(deadline)
}

impl DeviceNetworkService {
    pub(crate) fn local_resource_human(
        &self,
    ) -> Option<crate::runner::shared::external::LocalHuman> {
        let connection = self.shared_connection()?;
        let runtime = self.inner.shared_work.0.lock().ok()?;
        if runtime.binding != connection.binding
            || runtime
                .view
                .expires_at_ms
                .is_none_or(|until| until <= now_ms())
        {
            return None;
        }
        Some(crate::runner::shared::external::LocalHuman {
            actor_device_id: connection.client.device_id.clone(),
            principal_session: runtime.token.clone()?,
            project_id: runtime.view.selected_project_id.clone()?,
        })
    }
    fn shared_connection(&self) -> Option<Connection> {
        let state = self.inner.state.lock().unwrap();
        if state.closing || !matches!(state.status, "active" | "stopped") {
            return None;
        }
        let client = state.client.clone()?;
        use sha2::{Digest, Sha256};
        Some(Connection {
            hub_id: state.settings.hub_id.clone()?,
            hub_binding: format!(
                "{}|{:x}",
                state.settings.hub_id.as_deref().unwrap_or_default(),
                Sha256::digest(state.shared.ca_certificate_pem.as_bytes())
            ),
            binding: format!(
                "{}|{}|{}|{:x}",
                state.settings.hub_id.as_deref().unwrap_or_default(),
                client.device_id,
                client.endpoint(),
                Sha256::digest(state.shared.ca_certificate_pem.as_bytes())
            ),
            client,
        })
    }
    pub fn shared_work_projection(&self) -> SharedWorkProjection {
        let connection = self.shared_connection();
        let network = self.projection_now();
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        // A lost/replaced device identity must never expose the prior user's cached data.
        if connection.as_ref().map(|c| c.binding.as_str()) != Some(runtime.binding.as_str())
            && (runtime.token.is_some() || !runtime.view.projects.is_empty())
        {
            runtime.clear();
        } else if runtime
            .view
            .expires_at_ms
            .is_some_and(|time| time <= now_ms())
        {
            runtime.expire_to_read_only_projects();
        }
        if !runtime.provider_binding.is_empty()
            && connection.as_ref().map(|c| c.binding.as_str())
                != Some(runtime.provider_binding.as_str())
        {
            runtime.provider_binding.clear();
            runtime.view.provider_draft = None;
            runtime.view.provider_error = Some(
                "Hubまたは端末の接続設定が変わりました。親フォルダを選び直してください。".into(),
            );
        }
        runtime.view.enrollment = network.enrollment;
        runtime.view.enrollment_error = network.error;
        runtime.projection(connection.is_some(), network.hub_url)
    }
    fn shared_current(&self, connection: &Connection, generation: u64, query: u64) -> bool {
        if self
            .shared_connection()
            .is_none_or(|current| current.binding != connection.binding)
        {
            return false;
        }
        let runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.generation == generation && runtime.query == query
    }
    pub async fn shared_work_command(
        &self,
        expected_generation: &str,
        command: SharedWorkCommand,
    ) -> SharedWorkProjection {
        // Synchronize expiry/device identity before admitting any command.
        self.shared_work_projection();
        if matches!(
            &command,
            SharedWorkCommand::ProviderStatus
                | SharedWorkCommand::ProviderStart
                | SharedWorkCommand::ProviderPrepare { .. }
                | SharedWorkCommand::ProviderRemoveTemplate { .. }
                | SharedWorkCommand::ProviderInstall { .. }
                | SharedWorkCommand::ProviderOperation { .. }
        ) {
            return self
                .shared_provider_command(expected_generation, command)
                .await;
        }
        let Some(connection) = self.shared_connection() else {
            return self.shared_failure(RequestError::Unavailable);
        };
        let (generation, query, token) = {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if expected_generation != runtime.generation.to_string() {
                return runtime.projection(true, connection.client.endpoint());
            }
            runtime.begin_command(&command)
        };
        let result = self
            .shared_execute(&connection, generation, query, token.as_deref(), command)
            .await;
        if self.shared_current(&connection, generation, query) {
            if let Err(error) = result {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                if runtime.generation != generation || runtime.query != query {
                    drop(runtime);
                    return self.shared_work_projection();
                }
                if matches!(error, RequestError::Http(401 | 403)) {
                    runtime.clear();
                } else if matches!(
                    error,
                    RequestError::Unavailable | RequestError::Http(500..=599)
                ) {
                    runtime.view.projects_stale = !runtime.view.projects.is_empty();
                }
                runtime.view.error = Some(error.message().into());
            }
        }
        self.shared_work_projection()
    }
    fn shared_failure(&self, error: RequestError) -> SharedWorkProjection {
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if matches!(
            error,
            RequestError::Unavailable | RequestError::Http(500..=599)
        ) {
            runtime.view.projects_stale = !runtime.view.projects.is_empty();
        }
        runtime.view.error = Some(error.message().into());
        drop(runtime);
        self.shared_work_projection()
    }
    async fn shared_execute(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: Option<&str>,
        command: SharedWorkCommand,
    ) -> Result<(), RequestError> {
        let current_token = self
            .shared_authenticate(connection, generation, query, token)
            .await?;
        let token = current_token.as_str();
        match command {
            SharedWorkCommand::OpenManagement => {
                return self
                    .shared_open_management(connection, generation, query, token)
                    .await;
            }
            SharedWorkCommand::Project { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_current(generation, query)?;
                if !runtime.view.projects.iter().any(|p| p.id == project_id) {
                    return Err(RequestError::Local("所属プロジェクトを選択してください。"));
                }
                runtime.view.selected_project_id = Some(project_id);
                runtime.view.conversations.clear();
                runtime.view.conversation_revision.clear();
                runtime.view.selected_conversation_id = None;
                runtime.view.selected_job_id = None;
                runtime.view.status = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.conversation_history = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.history_before = None;
                runtime.before = None;
                runtime.environment_before = None;
            }
            SharedWorkCommand::NewConversation { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.view.selected_conversation_id = None;
                runtime.view.selected_job_id = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.conversation_history = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.history_before = None;
            }
            SharedWorkCommand::SelectConversation {
                project_id,
                conversation_id,
            } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                let conversation = runtime
                    .view
                    .conversations
                    .iter()
                    .find(|item| item.id == conversation_id)
                    .ok_or(RequestError::Local(
                        "会話の一覧が変わりました。最新の一覧を確認してください。",
                    ))?;
                let latest_job_id = conversation.latest_job_id.clone();
                runtime.view.selected_conversation_id = Some(conversation_id);
                runtime.view.selected_job_id = latest_job_id;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.conversation_history = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.history_before = None;
            }
            SharedWorkCommand::RenameConversation {
                project_id,
                conversation_id,
                title,
            } => {
                let title = title.trim();
                if !super::stable_id(&conversation_id) || title.is_empty() || title.len() > 256 {
                    return Err(RequestError::Invalid);
                }
                let expected_revision = {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    let conversation = runtime
                        .view
                        .conversations
                        .iter()
                        .find(|item| {
                            item.id == conversation_id && item.can_rename && !item.delete_pending
                        })
                        .ok_or(RequestError::Local(
                            "名称を変える会話の最新状態を確認してください。",
                        ))?;
                    conversation.revision
                };
                let acknowledgment: Value = request(
                    &connection.client,
                    &format!("conversations/{conversation_id}/rename"),
                    Some(token),
                    Some(json!({"project_id":project_id,"title":title,"expected_revision":expected_revision})),
                    &[],
                )
                .await?;
                if acknowledgment["id"] != conversation_id || acknowledgment["title"] != title {
                    return Err(RequestError::Invalid);
                }
            }
            SharedWorkCommand::DeleteConversation {
                project_id,
                conversation_id,
            } => {
                if !super::stable_id(&conversation_id) {
                    return Err(RequestError::Invalid);
                }
                let expected_revision = {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    let conversation = runtime
                        .view
                        .conversations
                        .iter()
                        .find(|item| {
                            item.id == conversation_id && item.can_delete && !item.delete_pending
                        })
                        .ok_or(RequestError::Local(
                            "削除する会話の最新状態を確認してください。",
                        ))?;
                    conversation.revision
                };
                let acknowledgment: Value = request(
                    &connection.client,
                    &format!("conversations/{conversation_id}/delete"),
                    Some(token),
                    Some(json!({"project_id":project_id,"request_id":ulid::Ulid::new().to_string(),"expected_revision":expected_revision})),
                    &[],
                )
                .await?;
                if acknowledgment["id"] != conversation_id
                    || acknowledgment["delete_pending"].as_bool().is_none()
                    || acknowledgment["deleted"].as_bool().is_none()
                {
                    return Err(RequestError::Invalid);
                }
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.view.feedback = Some(if acknowledgment["deleted"] == true {
                    "会話を削除しました。".into()
                } else {
                    "会話の停止を依頼しました。停止を確認すると一覧から削除されます。".into()
                });
            }
            SharedWorkCommand::LeaveProject { project_id } => {
                if !super::stable_id(&project_id) {
                    return Err(RequestError::Invalid);
                }
                // A receiver-only PC does not appear in the controller's /projects list.
                // Read its own current membership before reserving an irreversible leave.
                let current: CurrentDeviceProjects =
                    request(&connection.client, "device-projects", None, None, &[]).await?;
                let mut matches = current.projects.iter().filter(|project| {
                    project.id == project_id && (project.can_control || project.can_execute)
                });
                let current_generation = matches
                    .next()
                    .filter(|project| project.participation_generation > 0)
                    .ok_or(RequestError::Local(
                        "離脱するプロジェクトの最新状態を確認してください。",
                    ))?
                    .participation_generation;
                if matches.next().is_some() {
                    return Err(RequestError::Invalid);
                }
                let projected_generation = {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_current(generation, query)?;
                    runtime
                        .view
                        .projects
                        .iter()
                        .find(|project| project.id == project_id)
                        .map(|project| project.participation_generation)
                }
                .or_else(|| {
                    self.execution_projection()
                        .projects
                        .iter()
                        .find(|project| project.id == project_id && project.can_execute)
                        .map(|project| project.participation_generation)
                });
                if projected_generation != Some(current_generation) {
                    return Err(RequestError::Local(
                        "このPCのプロジェクト参加が更新されました。最新の状態を確認してください。",
                    ));
                }
                let receipt = {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_current(generation, query)?;
                    let user_id = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?
                        .user_id
                        .clone();
                    let participation_generation = current_generation;
                    let receipt = if let Some(pending) = runtime
                        .receipts
                        .pending_leave(&connection.hub_binding, &user_id)
                    {
                        if !matches!(&pending.operation, ReceiptOperation::LeaveProject { project_id: pending_id } if pending_id == &project_id)
                            || pending.payload["expected_participation_generation"]
                                != participation_generation
                        {
                            return Err(RequestError::Local(
                                "前のプロジェクトの離脱結果を確認してください。",
                            ));
                        }
                        pending.clone()
                    } else {
                        let receipt = Receipt {
                            hub: connection.hub_binding.clone(),
                            user_id,
                            operation: ReceiptOperation::LeaveProject {
                                project_id: project_id.clone(),
                            },
                            payload: json!({"expected_participation_generation":participation_generation}),
                        };
                        runtime.receipts.insert(receipt.clone())?;
                        receipt
                    };
                    runtime.view.leave_pending_project_id = Some(project_id.clone());
                    receipt
                };
                self.shared_retry_leave_receipt(connection, token, receipt)
                    .await?;
            }
            SharedWorkCommand::BindProjectFolder {
                project_id,
                environment_id,
                directory,
                access_mode,
                expected_directory,
            } => {
                {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    // An execution-only PC has no controller project in /projects.
                    // The execution owner below checks /device-projects, local consent,
                    // and the exact Hub environment before touching the Runner.
                    runtime.require_current(generation, query)?;
                }
                self.bind_project_folder(
                    &project_id,
                    &environment_id,
                    directory,
                    access_mode,
                    expected_directory,
                )
                .await
                .map_err(RequestError::Filesystem)?;
            }
            SharedWorkCommand::Detail { project_id, job_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                if !super::stable_id(&job_id) {
                    return Err(RequestError::Invalid);
                }
                runtime.view.selected_job_id = Some(job_id);
                runtime.view.selected_conversation_id = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.conversation_history = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.history_before = None;
            }
            SharedWorkCommand::NextJobs { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.before = runtime
                    .view
                    .status
                    .as_ref()
                    .and_then(|s| s.next_before.clone());
            }
            SharedWorkCommand::NextEnvironments { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.environment_before = runtime
                    .view
                    .status
                    .as_ref()
                    .and_then(|s| s.next_environment_before.clone());
            }
            SharedWorkCommand::Latest { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.before = None;
                runtime.environment_before = None;
            }
            SharedWorkCommand::Submit {
                project_id,
                title,
                prompt,
                start_before_ms,
            } => {
                let start_before_ms = submission_start_deadline(start_before_ms, now_ms())?;
                let title = if title.trim().is_empty() {
                    prompt
                        .trim()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(64)
                        .collect::<String>()
                } else {
                    title
                };
                if title.trim().is_empty()
                    || title.len() > 256
                    || prompt.trim().is_empty()
                    || prompt.len() > 32768
                {
                    return Err(RequestError::Local(
                        "件名は256バイト以内、依頼内容は32KiB以内で入力してください。",
                    ));
                }
                let receipt = {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    runtime.require_not_leaving(&project_id)?;
                    let participation_generation = runtime.view.projects.iter()
                        .find(|p| p.id == project_id && p.can_submit)
                        .map(|p| p.participation_generation)
                        .ok_or(RequestError::Local(
                            "このプロジェクトから依頼を送信できません。現在の参加とHub接続を確認してください。",
                        ))?;
                    let user_id = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?
                        .user_id
                        .clone();
                    let receipt = Receipt {
                        hub: connection.hub_binding.clone(),
                        user_id,
                        operation: ReceiptOperation::Submit,
                        payload: json!({"request_id":ulid::Ulid::new().to_string(),"project_id":project_id,"project_participation":participation_generation,"environment_id":"","title":title,"input":{"version":2,"prompt":prompt,"input_refs":runtime.view.inputs.iter().map(|a|a.id.as_str()).collect::<Vec<_>>()},"descendant_budget":8,"start_before_ms":start_before_ms}),
                    };
                    // Persist the original request before it can leave this process.
                    runtime.receipts.insert(receipt.clone())?;
                    receipt
                };
                self.shared_submit_receipt(connection, generation, query, token, receipt)
                    .await?;
            }
            SharedWorkCommand::Continue {
                project_id,
                job_id,
                expected_revision,
                prompt,
                start_before_ms,
            } => {
                let start_before_ms = submission_start_deadline(start_before_ms, now_ms())?;
                if prompt.trim().is_empty() || prompt.len() > 32768 {
                    return Err(RequestError::Invalid);
                }
                let receipt = {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    runtime.require_not_leaving(&project_id)?;
                    let participation_generation = runtime
                        .view
                        .projects
                        .iter()
                        .find(|p| p.id == project_id && p.can_submit)
                        .map(|p| p.participation_generation)
                        .ok_or(RequestError::Local(
                            "プロジェクトへの参加を確認してください。",
                        ))?;
                    if !runtime.view.detail.as_ref().is_some_and(|job| {
                        job.id == job_id
                            && job.revision == expected_revision
                            && job.can_continue
                            && runtime.view.conversations.iter().any(|conversation| {
                                conversation.id
                                    == job.conversation_id.as_deref().unwrap_or(&job.root_id)
                                    && conversation.latest_job_id.as_deref() == Some(&job_id)
                                    && !conversation.delete_pending
                            })
                    }) {
                        return Err(RequestError::Local(
                            "終了した仕事の最新状態を確認してください。",
                        ));
                    }
                    let conversation_epoch = runtime
                        .view
                        .detail
                        .as_ref()
                        .expect("validated continuation detail")
                        .conversation_epoch;
                    let user_id = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?
                        .user_id
                        .clone();
                    let receipt = Receipt {
                        hub: connection.hub_binding.clone(),
                        user_id,
                        operation: ReceiptOperation::Continue { job_id },
                        payload: json!({"request_id":ulid::Ulid::new().to_string(),"expected_revision":expected_revision,"conversation_epoch":conversation_epoch,"project_participation":participation_generation,"prompt":prompt,"input_refs":runtime.view.inputs.iter().map(|a|a.id.as_str()).collect::<Vec<_>>(),"start_before_ms":start_before_ms}),
                    };
                    runtime.receipts.insert(receipt.clone())?;
                    receipt
                };
                self.shared_submit_receipt(connection, generation, query, token, receipt)
                    .await?;
            }
            SharedWorkCommand::Revise {
                project_id,
                conversation_id,
                job_id,
                expected_revision,
                prompt,
            } => {
                if !super::stable_id(&conversation_id)
                    || !super::stable_id(&job_id)
                    || prompt.trim().is_empty()
                    || prompt.len() > 32768
                {
                    return Err(RequestError::Invalid);
                }
                let title = prompt
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(64)
                    .collect::<String>();
                let receipt = {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    runtime.require_not_leaving(&project_id)?;
                    let participation_generation = runtime
                        .view
                        .projects
                        .iter()
                        .find(|project| project.id == project_id && project.can_submit)
                        .map(|project| project.participation_generation)
                        .ok_or(RequestError::Local(
                            "プロジェクトへの参加を確認してください。",
                        ))?;
                    let detail = runtime.view.detail.as_ref().ok_or(RequestError::Local(
                        "編集する発言の最新状態を確認してください。",
                    ))?;
                    if detail.input["input_refs"]
                        .as_array()
                        .is_some_and(|refs| !refs.is_empty())
                    {
                        return Err(RequestError::Local(
                            "添付付きの依頼は編集できません。新しい依頼として送ってください。",
                        ));
                    }
                    if detail.id != job_id
                        || detail.project_id != project_id
                        || detail.conversation_id.as_deref().unwrap_or(&detail.root_id)
                            != conversation_id
                        || detail.revision != expected_revision
                        || !detail.can_revise
                        || !runtime.view.conversations.iter().any(|item| {
                            item.id == conversation_id
                                && item.latest_job_id.as_deref() == Some(&job_id)
                                && item.can_revise
                                && !item.delete_pending
                        })
                    {
                        return Err(RequestError::Local(
                            "停止した最新の発言だけを編集できます。現在の会話を確認してください。",
                        ));
                    }
                    let user_id = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?
                        .user_id
                        .clone();
                    let receipt = Receipt {
                        hub: connection.hub_binding.clone(),
                        user_id,
                        operation: ReceiptOperation::Submit,
                        payload: json!({
                            "request_id":ulid::Ulid::new().to_string(),
                            "project_id":project_id,
                            "project_participation":participation_generation,
                            "environment_id":"",
                            "conversation_id":conversation_id,
                            "revises_job_id":job_id,
                            "expected_revised_revision":expected_revision,
                            "title":title,
                            "input":{"version":2,"prompt":prompt,"input_refs":runtime.view.inputs.iter().map(|asset|asset.id.as_str()).collect::<Vec<_>>()},
                            "descendant_budget":8,
                            "start_before_ms":null
                        }),
                    };
                    runtime.receipts.insert(receipt.clone())?;
                    receipt
                };
                self.shared_submit_receipt(connection, generation, query, token, receipt)
                    .await?;
            }
            SharedWorkCommand::UploadInputs { project_id } => {
                self.shared_upload_inputs(connection, generation, query, token, &project_id)
                    .await?;
            }
            SharedWorkCommand::PrepareSample { project_id } => {
                self.shared_prepare_sample(connection, generation, query, token, &project_id)
                    .await?;
            }
            SharedWorkCommand::RemoveInput {
                project_id,
                asset_id,
            } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.view.inputs.retain(|asset| asset.id != asset_id);
            }
            SharedWorkCommand::SaveAsset {
                project_id,
                job_id,
                asset_id,
                import,
            } => {
                self.shared_save_asset(
                    connection,
                    generation,
                    query,
                    token,
                    &project_id,
                    &job_id,
                    &asset_id,
                    import,
                )
                .await?;
            }
            SharedWorkCommand::TranscriptNext { project_id, job_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                if runtime.view.selected_job_id.as_deref() != Some(&job_id) {
                    return Err(RequestError::Invalid);
                }
                runtime.transcript_after = runtime
                    .view
                    .transcript
                    .as_ref()
                    .and_then(|t| t.next_after)
                    .ok_or(RequestError::Invalid)?;
            }
            SharedWorkCommand::ConversationHistoryNext {
                project_id,
                conversation_id,
            } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                let history = runtime
                    .view
                    .conversation_history
                    .as_ref()
                    .ok_or(RequestError::Invalid)?;
                if history.project_id != project_id || history.conversation_id != conversation_id {
                    return Err(RequestError::Invalid);
                }
                runtime.history_before = Some(history.next_before.ok_or(RequestError::Invalid)?);
            }
            SharedWorkCommand::StopConversation {
                project_id,
                conversation_id,
            } => {
                if !super::stable_id(&conversation_id) {
                    return Err(RequestError::Invalid);
                }
                let receipt = {
                    let mut runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    let detail = runtime.view.detail.as_ref().ok_or(RequestError::Invalid)?;
                    if detail.project_id != project_id
                        || detail.conversation_id.as_deref().unwrap_or(&detail.root_id)
                            != conversation_id
                        || !runtime
                            .view
                            .projects
                            .iter()
                            .any(|project| project.id == project_id && project.can_submit)
                    {
                        return Err(RequestError::Local(
                            "表示中の会話と停止対象を確認してください。",
                        ));
                    }
                    let can_cancel = runtime.view.status.as_ref().is_some_and(|status| {
                        status.jobs.iter().any(|job| {
                            job.conversation_id.as_deref() == Some(&conversation_id)
                                && job.can_cancel
                        })
                    }) || runtime.view.conversation_history.as_ref().is_some_and(
                        |history| {
                            history.project_id == project_id
                                && history.conversation_id == conversation_id
                                && history.jobs.iter().any(|entry| entry.job.can_cancel)
                        },
                    );
                    let has_service = detail
                        .retained_services
                        .iter()
                        .any(|service| service.can_stop && !service.stop_requested);
                    if !can_cancel && !has_service {
                        return Err(RequestError::Local(
                            "この会話に停止対象はありません。最新の状態を確認してください。",
                        ));
                    }
                    let user_id = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?
                        .user_id
                        .clone();
                    let receipt = Receipt {
                        hub: connection.hub_binding.clone(),
                        user_id,
                        operation: ReceiptOperation::StopAllConversation { conversation_id },
                        payload: json!({"project_id":project_id,"request_id":ulid::Ulid::new().to_string()}),
                    };
                    runtime.receipts.insert(receipt.clone())?;
                    receipt
                };
                self.shared_submit_receipt(connection, generation, query, token, receipt)
                    .await?;
                self.inner.shared_work.0.lock().unwrap().view.feedback = Some(
                    "会話の停止をHubへ依頼しました。実行中の仕事とアプリが止まったか、画面で確認してください。"
                        .into(),
                );
            }
            SharedWorkCommand::Handover {
                project_id,
                job_id,
                expected_revision,
                new_assignee_id,
            } => {
                self.shared_handover(
                    connection,
                    generation,
                    query,
                    token,
                    &project_id,
                    &job_id,
                    expected_revision,
                    &new_assignee_id,
                )
                .await?;
            }
            SharedWorkCommand::InboxNext => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_current(generation, query)?;
                runtime.inbox_before = runtime
                    .view
                    .inbox
                    .as_ref()
                    .and_then(|v| v.next_before.clone());
            }
            SharedWorkCommand::InboxLatest => {
                self.inner.shared_work.0.lock().unwrap().inbox_before = None;
            }
            SharedWorkCommand::InboxOpen { notification_id } => {
                self.shared_inbox_open(connection, generation, query, token, &notification_id)
                    .await?;
            }
            SharedWorkCommand::RetrySubmission => {
                let receipt = {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_current(generation, query)?;
                    if let Some(error) = runtime.receipts.error() {
                        return Err(RequestError::Local(error));
                    }
                    let user = runtime
                        .view
                        .principal
                        .as_ref()
                        .ok_or(RequestError::Http(401))?;
                    runtime
                        .receipts
                        .pending(&connection.hub_binding, &user.user_id)
                        .cloned()
                        .ok_or(RequestError::Local("未確認の受付はありません。"))?
                };
                self.shared_submit_receipt(connection, generation, query, token, receipt)
                    .await?;
            }
            SharedWorkCommand::Cancel { project_id, job_id } => {
                {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    let listed = runtime
                        .view
                        .status
                        .as_ref()
                        .is_some_and(|s| s.jobs.iter().any(|j| j.id == job_id && j.can_cancel))
                        || runtime
                            .view
                            .conversation_history
                            .as_ref()
                            .is_some_and(|history| {
                                history.project_id == project_id
                                    && history
                                        .jobs
                                        .iter()
                                        .any(|entry| entry.job.id == job_id && entry.job.can_cancel)
                            });
                    if !listed {
                        return Err(RequestError::Local(
                            "最新の一覧から取消できる仕事を選択してください。",
                        ));
                    }
                }
                request::<Value>(
                    &connection.client,
                    &format!("jobs/{job_id}/cancel"),
                    Some(token),
                    Some(json!({})),
                    &[],
                )
                .await?;
            }
            SharedWorkCommand::StopService {
                project_id,
                service_id,
            } => {
                {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    if !super::stable_id(&service_id)
                        || !runtime.view.detail.as_ref().is_some_and(|detail| {
                            detail.project_id == project_id
                                && detail.retained_services.iter().any(|service| {
                                    service.service_id == service_id
                                        && service.can_stop
                                        && !service.stop_requested
                                })
                        })
                    {
                        return Err(RequestError::Local(
                            "停止するアプリの最新状態を確認してください。",
                        ));
                    }
                }
                request::<Value>(
                    &connection.client,
                    &format!("services/{service_id}/stop"),
                    Some(token),
                    Some(json!({})),
                    &[],
                )
                .await?;
            }
            SharedWorkCommand::Decide {
                project_id,
                job_id,
                approval_id,
                decision,
            } => {
                {
                    let runtime = self.inner.shared_work.0.lock().unwrap();
                    runtime.require_project(&project_id, generation, query)?;
                    if runtime.view.selected_job_id.as_deref() != Some(&job_id)
                        || !runtime.view.approval.as_ref().is_some_and(|a| {
                            a.id == approval_id
                                && a.status == "pending"
                                && a.can_decide
                                && a.expires_at_ms > now_ms()
                        })
                    {
                        return Err(RequestError::Local("最新の承認依頼を確認してください。"));
                    }
                }
                request::<Value>(
                    &connection.client,
                    &format!("jobs/{job_id}/approvals/{approval_id}/decision"),
                    Some(token),
                    Some(json!({"decision":decision})),
                    &[],
                )
                .await?;
            }
            _ => {}
        }
        self.shared_refresh(connection, generation, query, token)
            .await
    }
    async fn shared_retry_leave_receipt(
        &self,
        connection: &Connection,
        token: &str,
        receipt: Receipt,
    ) -> Result<(), RequestError> {
        let ReceiptOperation::LeaveProject { project_id } = &receipt.operation else {
            return Err(RequestError::Invalid);
        };
        let result: Result<Value, RequestError> = request(
            &connection.client,
            &receipt.operation.path(),
            Some(token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        if matches!(result, Err(RequestError::Http(404 | 409))) {
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)?;
            return Err(RequestError::Local(
                "前回の離脱は現在の参加に適用されませんでした。最新の状態を確認してください。",
            ));
        }
        let acknowledgment = result?;
        if acknowledgment["left"] != true
            || acknowledgment["project_id"] != project_id.as_str()
            || acknowledgment["participation_generation"]
                != receipt.payload["expected_participation_generation"]
        {
            return Err(RequestError::Invalid);
        }
        let same_hub = self
            .shared_connection()
            .as_ref()
            .is_some_and(|current| current.hub_binding == receipt.hub);
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.receipts.remove_confirmed(&receipt)?;
        if same_hub {
            runtime.view.leave_pending_project_id = None;
            runtime.view.feedback = Some("このPCはプロジェクトから離脱しました。".into());
        }
        Ok(())
    }
    async fn shared_submit_receipt(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        receipt: Receipt,
    ) -> Result<(), RequestError> {
        if let ReceiptOperation::StopConversation { conversation_id }
        | ReceiptOperation::StopAllConversation { conversation_id } = &receipt.operation
        {
            let result: Result<Value, RequestError> = request(
                &connection.client,
                &receipt.operation.path(),
                Some(token),
                Some(receipt.payload.clone()),
                &[],
            )
            .await;
            if matches!(result, Err(RequestError::Http(400 | 404))) {
                self.inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .receipts
                    .remove_confirmed(&receipt)?;
            }
            let acknowledgment = result?;
            if acknowledgment["project_id"] != receipt.payload["project_id"]
                || acknowledgment["conversation_id"].as_str() != Some(conversation_id)
                || acknowledgment["request_id"] != receipt.payload["request_id"]
                || acknowledgment["accepted"].as_bool() != Some(true)
                || (matches!(
                    &receipt.operation,
                    ReceiptOperation::StopAllConversation { .. }
                ) && acknowledgment["conversation_epoch"].as_u64().is_none())
            {
                return Err(RequestError::Invalid);
            }
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)?;
            return Ok(());
        }
        let result: Result<WorkDetail, RequestError> = request(
            &connection.client,
            &receipt.operation.path(),
            Some(token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        // Only this exact submission's definitive answer can retire its receipt.
        // Identity changes and unrelated failures leave it recoverable by the same actor.
        if result.is_ok()
            || matches!(&result, Err(RequestError::Http(400 | 404 | 409)))
            || (matches!(&receipt.operation, ReceiptOperation::Continue { .. })
                && matches!(&result, Err(RequestError::Http(409))))
        {
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)?;
        }
        let job = result?;
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if runtime.generation != generation || runtime.query != query {
            return Ok(());
        }
        runtime.view.selected_project_id = Some(job.project_id.clone());
        runtime.view.selected_job_id = Some(job.id.clone());
        let conversation_id = job.conversation_id.as_deref().unwrap_or(&job.root_id);
        runtime.view.selected_conversation_id = Some(conversation_id.to_owned());
        if runtime
            .view
            .conversation_history
            .as_ref()
            .is_some_and(|history| {
                history.project_id != job.project_id || history.conversation_id != conversation_id
            })
        {
            runtime.view.conversation_history = None;
        }
        runtime.view.detail = Some(job);
        runtime.view.inputs.clear();
        runtime.view.approval = None;
        runtime.view.handover = None;
        runtime.transcript_after = 0;
        runtime.history_before = None;
        runtime.before = None;
        runtime.environment_before = None;
        Ok(())
    }
    async fn shared_refresh(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
    ) -> Result<(), RequestError> {
        let pending_leave = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.view.principal.as_ref().and_then(|principal| {
                runtime
                    .receipts
                    .pending_leave(&connection.hub_binding, &principal.user_id)
                    .cloned()
            })
        };
        if let Some(receipt) = pending_leave {
            // The receipt is safe to retry: Hub compares the old participation generation.
            // A lost reply must not let a later rejoin inherit the old leave request.
            let _ = self
                .shared_retry_leave_receipt(connection, token, receipt)
                .await;
        }
        let session: Session =
            request(&connection.client, "session", Some(token), None, &[]).await?;
        let mut projects: Vec<WorkProject> =
            request(&connection.client, "projects", Some(token), None, &[]).await?;
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        for project in &mut projects {
            project.can_submit &= project.allows_submission();
        }
        if projects.len() > 1024
            || projects.iter().any(|project| {
                !super::stable_id(&project.id) || project.participation_generation == 0
            })
        {
            return Err(RequestError::Invalid);
        }
        let (project, before, environment_before, transcript_after, history_before, inbox_before) = {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.generation != generation || runtime.query != query {
                return Ok(());
            }
            if runtime
                .view
                .principal
                .as_ref()
                .is_none_or(|p| p.user_id != session.principal.user_id)
            {
                return Err(RequestError::Http(401));
            }
            runtime.view.principal = Some(session.principal);
            runtime.view.expires_at_ms = Some(session.expires_at_ms);
            runtime.view.project_access = session.project_access;
            if !projects
                .iter()
                .any(|p| Some(&p.id) == runtime.view.selected_project_id.as_ref())
            {
                runtime.view.selected_project_id = projects.first().map(|p| p.id.clone());
                runtime.view.conversations.clear();
                runtime.view.conversation_revision.clear();
                runtime.view.selected_conversation_id = None;
                runtime.view.selected_job_id = None;
                runtime.view.status = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.conversation_history = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.history_before = None;
                runtime.before = None;
                runtime.environment_before = None;
            }
            if runtime
                .view
                .leave_pending_project_id
                .as_ref()
                .is_some_and(|id| !projects.iter().any(|project| &project.id == id))
            {
                runtime.view.leave_pending_project_id = None;
            }
            runtime.view.projects = projects;
            runtime.view.projects_stale = false;
            (
                runtime.view.selected_project_id.clone(),
                runtime.before.clone(),
                runtime.environment_before.clone(),
                runtime.transcript_after,
                runtime.history_before,
                runtime.inbox_before.clone(),
            )
        };
        let mut inbox_params = vec![("limit", "30")];
        if let Some(before) = &inbox_before {
            inbox_params.push(("before", before));
        }
        let inbox: WorkInbox = request(
            &connection.client,
            "inbox",
            Some(token),
            None,
            &inbox_params,
        )
        .await?;
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        self.inner.shared_work.0.lock().unwrap().view.inbox = Some(inbox);
        if let Some(project) = project {
            let conversations: WorkConversationList = request(
                &connection.client,
                &format!("projects/{project}/conversations"),
                Some(token),
                None,
                &[],
            )
            .await?;
            if conversations.conversations.len() > 1024
                || conversations.conversations.iter().any(|item| {
                    !super::stable_id(&item.id)
                        || item.title.len() > 256
                        || item
                            .latest_job_id
                            .as_deref()
                            .is_some_and(|id| !super::stable_id(id))
                })
            {
                return Err(RequestError::Invalid);
            }
            if !self.shared_current(connection, generation, query) {
                return Ok(());
            }
            let job = {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project, generation, query)?;
                if let Some(selected) = runtime.view.selected_conversation_id.as_deref() {
                    if let Some(item) = conversations
                        .conversations
                        .iter()
                        .find(|item| item.id == selected)
                    {
                        let selected_child_is_current =
                            selected_child_matches_conversation(&runtime.view, &project, item);
                        if !selected_child_is_current {
                            runtime.view.selected_job_id = item.latest_job_id.clone();
                        }
                    } else {
                        runtime.view.selected_conversation_id = None;
                        runtime.view.selected_job_id = None;
                        runtime.view.detail = None;
                        runtime.view.approval = None;
                        runtime.view.conversation_history = None;
                    }
                }
                runtime.view.conversations = conversations.conversations;
                runtime.view.conversation_revision = conversations.revision;
                runtime.view.selected_job_id.clone()
            };
            let mut params = vec![("project_id", project.as_str()), ("limit", "50")];
            if let Some(before) = &before {
                params.push(("before", before));
            }
            if let Some(before) = &environment_before {
                params.push(("environment_before", before));
            }
            let status: WorkStatus =
                request(&connection.client, "status", Some(token), None, &params).await?;
            let (assets, transcript): (Vec<WorkAsset>, Option<WorkTranscript>) =
                if let Some(job) = &job {
                    (
                        request(
                            &connection.client,
                            &format!("jobs/{job}/assets"),
                            Some(token),
                            None,
                            &[],
                        )
                        .await?,
                        Some(
                            request(
                                &connection.client,
                                &format!("jobs/{job}/transcript"),
                                Some(token),
                                None,
                                &[("after", &transcript_after.to_string()), ("limit", "100")],
                            )
                            .await?,
                        ),
                    )
                } else {
                    (Vec::new(), None)
                };
            let handover: Option<WorkHandover> = if let Some(job) = &job {
                Some(
                    request(
                        &connection.client,
                        &format!("jobs/{job}/handover"),
                        Some(token),
                        None,
                        &[],
                    )
                    .await?,
                )
            } else {
                None
            };
            let (detail, approval): (Option<WorkDetail>, Option<WorkApproval>) = match job {
                Some(job) => (
                    Some(
                        request(
                            &connection.client,
                            &format!("jobs/{job}"),
                            Some(token),
                            None,
                            &[],
                        )
                        .await?,
                    ),
                    request(
                        &connection.client,
                        &format!("jobs/{job}/approval"),
                        Some(token),
                        None,
                        &[],
                    )
                    .await?,
                ),
                None => (None, None),
            };
            let history = if let Some(detail) = &detail {
                let conversation_id = detail.conversation_id.as_deref().unwrap_or(&detail.root_id);
                if !super::stable_id(conversation_id) {
                    return Err(RequestError::Invalid);
                }
                let old_snapshot = self
                    .inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .view
                    .conversation_history
                    .as_ref()
                    .filter(|old| {
                        old.project_id == project && old.conversation_id == conversation_id
                    })
                    .map(|old| old.snapshot);
                let before = history_before.filter(|_| old_snapshot.is_some());
                let before_text = before.map(|value| value.to_string());
                let snapshot_text = before.and(old_snapshot).map(|value| value.to_string());
                let mut params = vec![("project_id", project.as_str()), ("limit", "20")];
                if let Some(value) = before_text.as_deref() {
                    params.push(("before", value));
                }
                if let Some(value) = snapshot_text.as_deref() {
                    params.push(("snapshot", value));
                }
                let response: Result<WorkConversationHistory, RequestError> = request(
                    &connection.client,
                    &format!("conversations/{conversation_id}/history"),
                    Some(token),
                    None,
                    &params,
                )
                .await;
                match response {
                    Ok(page) => {
                        if page.project_id != project
                            || page.conversation_id != conversation_id
                            || page.jobs.len() > 20
                            || before.is_some_and(|cursor| page.next_before == Some(cursor))
                            || page.jobs.iter().any(|entry| {
                                entry.job.parent_id.is_some()
                                    || entry.job.conversation_id.as_deref() != Some(conversation_id)
                            })
                        {
                            return Err(RequestError::Invalid);
                        }
                        Some((page, before.is_some()))
                    }
                    Err(RequestError::Http(404)) if before.is_none() => None,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            if !self.shared_current(connection, generation, query) {
                return Ok(());
            }
            if status.project_id != project
                || detail.as_ref().is_some_and(|d| d.project_id != project)
            {
                return Err(RequestError::Invalid);
            }
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.generation != generation || runtime.query != query {
                return Ok(());
            }
            runtime.view.status = Some(status);
            if let Some(detail) = &detail {
                runtime.view.selected_conversation_id = Some(
                    detail
                        .conversation_id
                        .clone()
                        .unwrap_or_else(|| detail.root_id.clone()),
                );
            }
            runtime.view.detail = detail;
            runtime.view.approval = approval;
            runtime.view.assets = assets;
            runtime.view.transcript = transcript;
            runtime.view.conversation_history = history.map(|(page, older)| {
                merge_conversation_history(runtime.view.conversation_history.as_ref(), page, older)
            });
            runtime.history_before = None;
            runtime.view.handover = handover;
            runtime.view.observed_at_ms = Some(now_ms());
        }
        Ok(())
    }
}
async fn request<T: DeserializeOwned>(
    client: &DeviceClient,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    query: &[(&str, &str)],
) -> Result<T, RequestError> {
    let max_bytes = if path.ends_with("/transcript") {
        65 * 1024 * 1024
    } else if path.starts_with("assets/")
        || (path.starts_with("runner/attempts/")
            && path.contains("/children/")
            && path.contains("/artifacts/"))
    {
        12 * 1024 * 1024
    } else {
        4 * 1024 * 1024
    };
    let mut url = reqwest::Url::parse(&format!("{}/v1/shared/{path}", client.endpoint()))
        .map_err(|_| RequestError::Invalid)?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    tokio::time::timeout(Duration::from_secs(15), async {
        let lease = client.http().acquire().await;
        let mut request = if let Some(body) = body {
            lease.http.post(url).json(&body)
        } else {
            lease.http.get(url)
        };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|_| RequestError::Unavailable)?;
        if !response.status().is_success() {
            return Err(RequestError::Http(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > max_bytes as u64)
        {
            return Err(RequestError::Invalid);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| RequestError::Unavailable)?;
            if bytes.len() + chunk.len() > max_bytes {
                return Err(RequestError::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| RequestError::Invalid)
    })
    .await
    .map_err(|_| RequestError::Unavailable)?
}

fn merge_conversation_history(
    previous: Option<&WorkConversationHistory>,
    mut page: WorkConversationHistory,
    older: bool,
) -> WorkConversationHistory {
    let Some(previous) = previous.filter(|old| {
        old.project_id == page.project_id && old.conversation_id == page.conversation_id
    }) else {
        return page;
    };
    if older && previous.snapshot != page.snapshot {
        return page;
    }
    let mut seen = std::collections::HashSet::new();
    if older {
        let mut jobs = previous.jobs.clone();
        seen.extend(jobs.iter().map(|entry| entry.job.id.clone()));
        jobs.extend(
            page.jobs
                .into_iter()
                .filter(|entry| seen.insert(entry.job.id.clone())),
        );
        page.jobs = jobs;
    } else {
        seen.extend(page.jobs.iter().map(|entry| entry.job.id.clone()));
        let has_loaded_older_rows = previous
            .jobs
            .iter()
            .any(|entry| !seen.contains(&entry.job.id));
        page.jobs.extend(
            previous
                .jobs
                .iter()
                .filter(|entry| seen.insert(entry.job.id.clone()))
                .cloned(),
        );
        // Earlier pages already loaded by the user remain available during polling.
        if has_loaded_older_rows {
            page.next_before = previous.next_before;
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_job_may_omit_pc_for_hub_placement() {
        let command: SharedWorkCommand = serde_json::from_value(json!({
            "kind":"submit", "project_id":"project", "title":"", "prompt":"Create a TODO app"
        }))
        .unwrap();
        assert!(matches!(command, SharedWorkCommand::Submit { .. }));
    }

    #[test]
    fn child_selection_requires_the_same_project_conversation_and_latest_root() {
        let detail: WorkDetail = serde_json::from_value(json!({
            "id":"child-a","project_id":"project-a","conversation_id":"root-a",
            "root_id":"root-a","parent_id":"root-a","environment_id":"env-b",
            "title":"Child","input":{},"result":null,"state":"running",
            "awaiting_child_id":null,"revision":1,"created_at_ms":1,"updated_at_ms":1
        }))
        .unwrap();
        let mut view = SharedWorkProjection {
            selected_job_id: Some("child-a".into()),
            detail: Some(detail),
            ..SharedWorkProjection::default()
        };
        let mut conversation = WorkConversation {
            id: "root-a".into(),
            title: "Parent".into(),
            latest_job_id: Some("root-a".into()),
            updated_at_ms: 1,
            revision: 1,
            delete_pending: false,
            can_rename: false,
            can_delete: false,
            can_revise: false,
        };
        assert!(selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
        view.detail.as_mut().unwrap().conversation_id = None;
        assert!(selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
        view.detail.as_mut().unwrap().state = "succeeded".into();
        assert!(!selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
        view.detail.as_mut().unwrap().state = "running".into();
        assert!(!selected_child_matches_conversation(
            &view,
            "project-b",
            &conversation
        ));
        conversation.id = "other-conversation".into();
        assert!(!selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
        conversation.id = "root-a".into();
        conversation.latest_job_id = Some("new-root".into());
        assert!(!selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
        conversation.latest_job_id = Some("root-a".into());
        view.selected_job_id = Some("other-child".into());
        assert!(!selected_child_matches_conversation(
            &view,
            "project-a",
            &conversation
        ));
    }

    #[test]
    fn stopping_retained_service_requires_exact_project_and_handle() {
        assert!(
            serde_json::from_value::<SharedWorkCommand>(json!({
                "kind":"stop_service", "project_id":"project", "service_id":"service"
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<SharedWorkCommand>(json!({
                "kind":"stop_service", "project_id":"project"
            }))
            .is_err()
        );
    }

    fn history_page(
        ids: &[&str],
        snapshot: u64,
        next_before: Option<u64>,
    ) -> WorkConversationHistory {
        WorkConversationHistory {
            project_id: "project-a".into(),
            conversation_id: "conversation-a".into(),
            snapshot,
            conversation_epoch: 0,
            jobs: ids
                .iter()
                .map(|id| WorkConversationJob {
                    job: serde_json::from_value(json!({
                        "id": id, "conversation_id": "conversation-a", "root_id": "root-a",
                        "parent_id": null, "title": id, "state": "succeeded",
                        "environment_id": "env-a", "environment_label": "WinB",
                        "requestor": {"user_id": "user-a", "display_name": "User A"},
                        "assignee": {"user_id": "user-a", "display_name": "User A"},
                        "wait_reason": null, "uncertainty_reason": null,
                        "can_cancel": false, "revision": 1, "created_at_ms": 1, "updated_at_ms": 1
                    }))
                    .unwrap(),
                    input: json!({"prompt":id}),
                    result: Some(json!({"text":id})),
                    artifacts: Vec::new(),
                    more_artifacts: false,
                })
                .collect(),
            next_before,
        }
    }

    #[test]
    fn conversation_history_keeps_older_pages_and_newer_poll_rows_once() {
        let first = history_page(&["job-c", "job-b"], 10, Some(8));
        let older = history_page(&["job-a"], 10, None);
        let loaded = merge_conversation_history(Some(&first), older, true);
        assert_eq!(
            loaded
                .jobs
                .iter()
                .map(|entry| entry.job.id.as_str())
                .collect::<Vec<_>>(),
            ["job-c", "job-b", "job-a"]
        );
        assert_eq!(loaded.next_before, None);
        let refreshed = merge_conversation_history(
            Some(&loaded),
            history_page(&["job-d", "job-c"], 11, Some(9)),
            false,
        );
        assert_eq!(
            refreshed
                .jobs
                .iter()
                .map(|entry| entry.job.id.as_str())
                .collect::<Vec<_>>(),
            ["job-d", "job-c", "job-b", "job-a"]
        );
        assert_eq!(refreshed.next_before, None);
        let mut foreign = history_page(&["other"], 12, None);
        foreign.conversation_id = "conversation-b".into();
        assert_eq!(
            merge_conversation_history(Some(&refreshed), foreign, false)
                .jobs
                .len(),
            1
        );
    }

    #[test]
    fn history_pagination_command_requires_exact_conversation_target() {
        let command: SharedWorkCommand = serde_json::from_value(json!({
            "kind":"conversation_history_next", "project_id":"project-a", "conversation_id":"conversation-a"
        })).unwrap();
        assert!(
            matches!(command, SharedWorkCommand::ConversationHistoryNext { project_id, conversation_id }
            if project_id == "project-a" && conversation_id == "conversation-a")
        );
    }
}
