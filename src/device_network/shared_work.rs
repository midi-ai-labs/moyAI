//! Human sessions and shared-work snapshots belong to the registered device runtime.
//! Remembered human credentials are Windows-user protected; the webview receives
//! only a typed projection, never access or refresh credentials.
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{DeviceClient, DeviceNetworkService};
mod auth_store;
mod authentication;
mod collaboration;
mod files;
mod providing;
mod receipts;
pub use collaboration::{WorkHandover, WorkInbox};
pub use files::WorkAsset;
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
    pub can_handover: bool,
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
    pub can_handover: bool,
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
    pub selected_project_id: Option<String>,
    pub selected_job_id: Option<String>,
    pub status: Option<WorkStatus>,
    pub detail: Option<WorkDetail>,
    pub approval: Option<WorkApproval>,
    pub inputs: Vec<WorkAsset>,
    pub assets: Vec<WorkAsset>,
    pub transcript: Option<WorkTranscript>,
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
    Login {
        username: String,
        password: String,
    },
    Logout,
    RetrySubmission,
    Refresh,
    Project {
        project_id: String,
    },
    NewConversation {
        project_id: String,
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
        environment_id: String,
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
    UploadInputs {
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
    #[serde(default)]
    refresh_token: Option<String>,
}
#[derive(Deserialize)]
struct Session {
    principal: WorkPrincipal,
    expires_at_ms: u64,
}
pub(super) struct SharedWorkOwner(Mutex<Runtime>);
impl SharedWorkOwner {
    pub fn new(path: camino::Utf8PathBuf) -> Self {
        Self(Mutex::new(Runtime {
            auth_store: auth_store::AuthStore::new(path.with_file_name("shared-human-auth.dpapi")),
            receipts: ReceiptStore::new(path),
            ..Runtime::default()
        }))
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
    remembered_refresh: Option<String>,
    auth_store: auth_store::AuthStore,
    view: SharedWorkProjection,
    before: Option<String>,
    environment_before: Option<String>,
    transcript_after: u64,
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
        if matches!(command, SharedWorkCommand::Logout) {
            self.clear();
        }
        captured
    }
    fn clear(&mut self) {
        self.generation += 1;
        self.query += 1;
        self.token = None;
        self.remembered_refresh = None;
        self.view = SharedWorkProjection::default();
        self.before = None;
        self.environment_before = None;
        self.transcript_after = 0;
        self.inbox_before = None;
        self.provider_binding.clear();
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
}
struct Connection {
    client: DeviceClient,
    hub_id: String,
    binding: String,
    hub_binding: String,
}
impl Connection {
    fn auth_binding(&self) -> String {
        format!("{}|{}", self.hub_binding, self.client.device_id)
    }
}
#[derive(Debug)]
enum RequestError {
    SignInRequired,
    Local(&'static str),
    Http(u16),
    Unavailable,
    Invalid,
    Filesystem(String),
}
impl RequestError {
    fn message(&self) -> &str {
        match self {
            Self::SignInRequired => "Hubの利用者アカウントでログインしてください。",
            Self::Local(message) => message,
            Self::Filesystem(message) => message,
            Self::Http(401) => {
                "利用者名とパスワードを確認してください。ログイン済みの場合は再度ログインしてください。"
            }
            Self::Http(403) => "この操作の権限または端末の利用許可を確認できません。",
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
        if let Some(refresh) = runtime.remembered_refresh.clone() {
            match runtime.auth_store.load() {
                Ok(doc)
                    if doc
                        .active
                        .as_ref()
                        .is_some_and(|a| a.refresh_token == refresh) => {}
                Ok(_) => runtime.clear(),
                Err(error) => {
                    runtime.clear();
                    runtime.view.error = Some(error.message().into());
                }
            }
        }
        // A lost/replaced device identity must never expose the prior user's cached data.
        if (connection.as_ref().map(|c| c.binding.as_str()) != Some(runtime.binding.as_str())
            && runtime.token.is_some())
            || runtime
                .view
                .expires_at_ms
                .is_some_and(|time| time <= now_ms())
        {
            runtime.clear();
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
        if matches!(command, SharedWorkCommand::Logout) {
            return self.shared_logout(expected_generation).await;
        }
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
                }
                runtime.view.error = Some(error.message().into());
            }
        }
        self.shared_work_projection()
    }
    fn shared_failure(&self, error: RequestError) -> SharedWorkProjection {
        self.inner.shared_work.0.lock().unwrap().view.error = Some(error.message().into());
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
        if let SharedWorkCommand::Login { username, password } = command {
            return self
                .shared_login(connection, generation, query, username, password)
                .await;
        }
        let current_token = match self
            .shared_authenticate(connection, generation, query, token)
            .await
        {
            Ok(token) => token,
            Err(RequestError::SignInRequired) if matches!(command, SharedWorkCommand::Refresh) => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let token = current_token.as_str();
        match command {
            SharedWorkCommand::Project { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_current(generation, query)?;
                if !runtime.view.projects.iter().any(|p| p.id == project_id) {
                    return Err(RequestError::Local("所属プロジェクトを選択してください。"));
                }
                runtime.view.selected_project_id = Some(project_id);
                runtime.view.selected_job_id = None;
                runtime.view.status = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.before = None;
                runtime.environment_before = None;
            }
            SharedWorkCommand::NewConversation { project_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                runtime.view.selected_job_id = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
            }
            SharedWorkCommand::Detail { project_id, job_id } => {
                let mut runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(&project_id, generation, query)?;
                if !super::stable_id(&job_id) {
                    return Err(RequestError::Invalid);
                }
                runtime.view.selected_job_id = Some(job_id);
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
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
                environment_id,
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
                    if !runtime
                        .view
                        .projects
                        .iter()
                        .any(|p| p.id == project_id && p.can_submit)
                        || !runtime.view.status.as_ref().is_some_and(|s| {
                            s.environments
                                .iter()
                                .any(|e| e.id == environment_id && e.enabled)
                        })
                    {
                        return Err(RequestError::Local(
                            "投入できるプロジェクトと実行環境を選択してください。",
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
                        payload: json!({"request_id":ulid::Ulid::new().to_string(),"project_id":project_id,"environment_id":environment_id,"title":title,"input":{"version":2,"prompt":prompt,"input_refs":runtime.view.inputs.iter().map(|a|a.id.as_str()).collect::<Vec<_>>()},"descendant_budget":8,"start_before_ms":start_before_ms}),
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
                    if !runtime
                        .view
                        .projects
                        .iter()
                        .any(|p| p.id == project_id && p.can_submit)
                        || !runtime.view.detail.as_ref().is_some_and(|job| {
                            job.id == job_id
                                && job.revision == expected_revision
                                && job.can_continue
                        })
                    {
                        return Err(RequestError::Local(
                            "終了した仕事の最新状態を確認してください。",
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
                        operation: ReceiptOperation::Continue { job_id },
                        payload: json!({"request_id":ulid::Ulid::new().to_string(),"expected_revision":expected_revision,"prompt":prompt,"input_refs":runtime.view.inputs.iter().map(|a|a.id.as_str()).collect::<Vec<_>>(),"start_before_ms":start_before_ms}),
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
                    if !runtime
                        .view
                        .status
                        .as_ref()
                        .is_some_and(|s| s.jobs.iter().any(|j| j.id == job_id && j.can_cancel))
                    {
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
    async fn shared_submit_receipt(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        receipt: Receipt,
    ) -> Result<(), RequestError> {
        let result: Result<WorkDetail, RequestError> = request(
            &connection.client,
            &receipt.operation.path(),
            Some(token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        // Only this exact submission's definitive answer can retire its receipt.
        // Logout and unrelated failed operations leave it recoverable after restart.
        if result.is_ok() || matches!(result, Err(RequestError::Http(400 | 404))) {
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
        runtime.view.detail = Some(job);
        runtime.view.inputs.clear();
        runtime.view.approval = None;
        runtime.view.handover = None;
        runtime.transcript_after = 0;
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
        let session: Session =
            request(&connection.client, "session", Some(token), None, &[]).await?;
        let mut projects: Vec<WorkProject> =
            request(&connection.client, "projects", Some(token), None, &[]).await?;
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        for project in &mut projects {
            project.can_submit = matches!(project.role.as_str(), "contributor" | "manager");
        }
        let (project, job, before, environment_before, transcript_after, inbox_before) = {
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
            if !projects
                .iter()
                .any(|p| Some(&p.id) == runtime.view.selected_project_id.as_ref())
            {
                runtime.view.selected_project_id = projects.first().map(|p| p.id.clone());
                runtime.view.selected_job_id = None;
                runtime.view.status = None;
                runtime.view.detail = None;
                runtime.view.approval = None;
                runtime.view.inputs.clear();
                runtime.view.assets.clear();
                runtime.view.transcript = None;
                runtime.view.handover = None;
                runtime.transcript_after = 0;
                runtime.before = None;
                runtime.environment_before = None;
            }
            runtime.view.projects = projects;
            (
                runtime.view.selected_project_id.clone(),
                runtime.view.selected_job_id.clone(),
                runtime.before.clone(),
                runtime.environment_before.clone(),
                runtime.transcript_after,
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
            runtime.view.detail = detail;
            runtime.view.approval = approval;
            runtime.view.assets = assets;
            runtime.view.transcript = transcript;
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
    } else if path.starts_with("assets/") {
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
