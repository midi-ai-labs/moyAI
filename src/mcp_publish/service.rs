use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use super::credentials::CredentialStore;
use super::dispatch::{
    PublishCallError, PublishReadDispatcher, PublishToolDispatcher, validate_target,
};
use super::transport::{PublishCallRecord, PublishHttpServer, PublishTransportObserver};
use super::{
    PublishAuthentication, PublishBackgroundPolicy, PublishError, PublishMode, PublishProfile,
    PublishProfileId, PublishProfileSet, PublishProfileStore, PublishTarget, PublishTls,
};
use crate::config::ResolvedConfig;
use crate::session::{ProjectRepository, SessionRepository};
use crate::storage::StoreBundle;
use crate::tool::ToolName;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishDraft {
    pub label: String,
    pub bind: SocketAddr,
    #[serde(default)]
    pub tls: Option<PublishTls>,
    #[serde(default)]
    pub mode: PublishMode,
    pub target: PublishTarget,
    pub tools: Vec<ToolName>,
    pub max_concurrent_calls: u16,
    pub background: PublishBackgroundPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishTargetChoice {
    pub target: PublishTarget,
    pub label: String,
}

impl PublishTargetChoice {
    fn temporary() -> Self {
        Self {
            target: PublishTarget::Temp {},
            label: "temp".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishProfileRow {
    pub profile: PublishProfile,
    pub status: PublishStatus,
    pub status_message: Option<String>,
    pub endpoint: Option<String>,
    pub active_calls: usize,
    pub connected_sessions: usize,
    pub recent_calls: Vec<PublishCallRecord>,
    pub credential_configured: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_issue_token: bool,
    pub can_revoke_token: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishProjection {
    pub revision: String,
    pub generation: String,
    pub profiles: Vec<PublishProfileRow>,
    pub targets: Vec<PublishTargetChoice>,
    pub error: Option<String>,
}

/// The token deliberately has no Debug implementation and never enters a projection.
#[derive(Serialize)]
pub struct PublishTokenReceipt {
    pub projection: PublishProjection,
    pub profile_id: PublishProfileId,
    pub token: String,
}

struct ProfileRuntime {
    status: PublishStatus,
    message: Option<String>,
    endpoint: Option<String>,
    server: Option<PublishHttpServer>,
    observer: Option<PublishTransportObserver>,
}

impl Default for ProfileRuntime {
    fn default() -> Self {
        Self {
            status: PublishStatus::Stopped,
            message: None,
            endpoint: None,
            server: None,
            observer: None,
        }
    }
}

struct PublishState {
    profiles: PublishProfileSet,
    generation: u64,
    runtimes: HashMap<PublishProfileId, ProfileRuntime>,
    targets: Vec<PublishTargetChoice>,
    error: Option<String>,
    closing: bool,
    hidden: bool,
}

/// One command lane owns persistence and listener lifecycle; projection reads never await it.
#[derive(Clone)]
pub struct PublishService {
    state: Arc<Mutex<PublishState>>,
    commands: Arc<AsyncMutex<()>>,
    profiles: PublishProfileStore,
    credentials: CredentialStore,
    store: StoreBundle,
    config: ResolvedConfig,
    protected_roots: Vec<Utf8PathBuf>,
    tls_directory: Utf8PathBuf,
    remote_jobs: Option<crate::remote_agent::RemoteJobService>,
}

impl std::fmt::Debug for PublishService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishService")
            .finish_non_exhaustive()
    }
}

impl PublishService {
    pub fn new(path: Utf8PathBuf, store: StoreBundle, config: ResolvedConfig) -> Self {
        let parent = path
            .parent()
            .expect("application config directory")
            .to_owned();
        let credentials = CredentialStore::new(parent.join("mcp-publish-credentials"));
        let profiles = PublishProfileStore::new(path);
        let (document, error) = match profiles.load() {
            Ok(document) => (document, None),
            Err(_) => (
                PublishProfileSet::default(),
                Some(
                    "配信設定を読み込めません。設定ファイルを確認してから再取得してください。"
                        .into(),
                ),
            ),
        };
        Self {
            state: Arc::new(Mutex::new(PublishState {
                profiles: document,
                generation: 0,
                runtimes: HashMap::new(),
                targets: vec![PublishTargetChoice::temporary()],
                error,
                closing: false,
                hidden: false,
            })),
            commands: Arc::new(AsyncMutex::new(())),
            profiles,
            credentials,
            tls_directory: parent.join("mcp-publish-tls"),
            protected_roots: vec![parent, store.paths().data_dir.clone()],
            remote_jobs: None,
            store,
            config,
        }
    }

    pub(crate) fn with_remote_jobs(mut self, jobs: crate::remote_agent::RemoteJobService) -> Self {
        self.remote_jobs = Some(jobs);
        self
    }

    pub fn projection_now(&self) -> PublishProjection {
        let state = self.state.lock().expect("publish state mutex poisoned");
        let writable = state.error.is_none() && !state.closing;
        let profiles = state
            .profiles
            .profiles
            .iter()
            .map(|profile| {
                let runtime = state.runtimes.get(&profile.id);
                let status = runtime.map_or(PublishStatus::Stopped, |value| value.status);
                let active = matches!(
                    status,
                    PublishStatus::Starting | PublishStatus::Running | PublishStatus::Stopping
                );
                let credential_configured = match profile.authentication {
                    PublishAuthentication::LocalCredential { credential_id } => {
                        self.credentials.verifier(profile.id, credential_id).is_ok()
                    }
                    PublishAuthentication::Unpaired {} => false,
                };
                let snapshot = runtime
                    .and_then(|value| value.observer.as_ref())
                    .map(|value| value.snapshot());
                let listener_failed = status == PublishStatus::Running
                    && snapshot.as_ref().is_some_and(|value| !value.live);
                let valid_target = state
                    .targets
                    .iter()
                    .any(|choice| choice.target == profile.target);
                PublishProfileRow {
                    profile: profile.clone(),
                    status: if listener_failed {
                        PublishStatus::Error
                    } else {
                        status
                    },
                    status_message: if listener_failed {
                        Some("受付処理が終了しました。停止してから再度開始してください。".into())
                    } else {
                        runtime.and_then(|value| value.message.clone())
                    },
                    endpoint: runtime.and_then(|value| value.endpoint.clone()),
                    active_calls: snapshot.as_ref().map_or(0, |value| value.active_calls),
                    connected_sessions: snapshot.as_ref().map_or(0, |value| value.sessions),
                    recent_calls: snapshot.map_or_else(Vec::new, |value| value.recent_calls),
                    credential_configured,
                    can_edit: writable && !active,
                    can_delete: writable && !active,
                    can_start: writable
                        && !active
                        && !state.hidden
                        && credential_configured
                        && valid_target
                        && profile.has_public_operations()
                        && (matches!(profile.mode, PublishMode::ReadTools {})
                            || self.remote_jobs.is_some()),
                    can_stop: active && status != PublishStatus::Starting,
                    can_issue_token: writable && !active,
                    can_revoke_token: writable
                        && profile.authentication != PublishAuthentication::Unpaired {},
                }
            })
            .collect();
        PublishProjection {
            revision: state.profiles.revision.to_string(),
            generation: state.generation.to_string(),
            profiles,
            targets: state.targets.clone(),
            error: state.error.clone(),
        }
    }

    pub fn polling_required(&self) -> bool {
        self.state
            .lock()
            .expect("publish state mutex poisoned")
            .runtimes
            .values()
            .any(|runtime| {
                matches!(
                    runtime.status,
                    PublishStatus::Starting | PublishStatus::Running | PublishStatus::Stopping
                )
            })
    }

    pub async fn refresh(&self) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let busy = self
            .state
            .lock()
            .expect("publish state mutex poisoned")
            .runtimes
            .values()
            .any(|runtime| runtime.server.is_some());
        if !busy {
            let loaded = self.profiles.load().map_err(public_error)?;
            let mut state = self.state.lock().expect("publish state mutex poisoned");
            if state.profiles != loaded {
                state.generation += 1;
            }
            state.profiles = loaded;
            state.error = None;
        }
        let store = self.store.clone();
        let protected = self.protected_roots.clone();
        // The validated profile set is bounded to 32 entries. Retain saved
        // projects beyond the list page and exact legacy chat scopes, but never
        // turn an old chat scope into broader project authority implicitly.
        let saved_targets = self
            .state
            .lock()
            .expect("publish state mutex poisoned")
            .profiles
            .profiles
            .iter()
            .map(|profile| profile.target.clone())
            .collect::<Vec<_>>();
        let targets = tokio::task::spawn_blocking(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "対象一覧を取得できません。".to_string())?
                .block_on(async move {
                    let mut targets = vec![PublishTargetChoice::temporary()];
                    let projects = store
                        .project_repo()
                        .list_projects(200)
                        .await
                        .map_err(|_| "対象一覧を取得できません。".to_string())?;
                    for project in projects {
                        let target = PublishTarget::Project {
                            project_id: project.id,
                            workspace_root: project.root_path,
                        };
                        // Internal Desktop workspaces are inside the protected
                        // data directory; the same boundary also governs save/start.
                        if validate_target(&store, &target, &protected).await.is_ok() {
                            targets.push(PublishTargetChoice {
                                target,
                                label: project.display_name,
                            });
                        }
                    }
                    for target in saved_targets {
                        if targets.iter().any(|choice| choice.target == target) {
                            continue;
                        }
                        if validate_target(&store, &target, &protected).await.is_err() {
                            continue;
                        }
                        let label = match &target {
                            PublishTarget::Project { project_id, .. } => {
                                let Ok(project) =
                                    store.project_repo().get_project(*project_id).await
                                else {
                                    continue;
                                };
                                project.display_name
                            }
                            PublishTarget::LegacySession {
                                project_id,
                                root_session_id,
                                ..
                            } => {
                                let Ok(project) =
                                    store.project_repo().get_project(*project_id).await
                                else {
                                    continue;
                                };
                                let Ok(session) =
                                    store.session_repo().get_session(*root_session_id).await
                                else {
                                    continue;
                                };
                                format!(
                                    "以前のチャット対象: {} / {}",
                                    project.display_name, session.title
                                )
                            }
                            PublishTarget::Temp {} => continue,
                        };
                        targets.push(PublishTargetChoice { target, label });
                    }
                    Ok::<_, String>(targets)
                })
        })
        .await
        .map_err(|_| "対象一覧を取得できません。".to_string())??;
        self.state
            .lock()
            .expect("publish state mutex poisoned")
            .targets = targets;
        Ok(self.projection_now())
    }

    fn checked(&self, revision: &str, generation: &str) -> Result<PublishProfileSet, String> {
        let state = self.state.lock().expect("publish state mutex poisoned");
        if state.closing {
            return Err("アプリを終了しています。".into());
        }
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        if parse_revision(revision)? != state.profiles.revision
            || parse_revision(generation)? != state.generation
        {
            return Err("配信設定または状態が変わりました。最新の状態を確認してください。".into());
        }
        Ok(state.profiles.clone())
    }

    fn profile(
        document: &PublishProfileSet,
        id: PublishProfileId,
    ) -> Result<PublishProfile, String> {
        document
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| "配信設定が見つかりません。".into())
    }

    fn require_stopped(&self, id: PublishProfileId) -> Result<(), String> {
        if self
            .state
            .lock()
            .expect("publish state mutex poisoned")
            .runtimes
            .get(&id)
            .is_some_and(|runtime| {
                matches!(
                    runtime.status,
                    PublishStatus::Starting | PublishStatus::Running | PublishStatus::Stopping
                )
            })
        {
            return Err("配信を停止してから変更してください。".into());
        }
        Ok(())
    }

    fn persist(&self, proposed: &PublishProfileSet) -> Result<(), String> {
        let saved = self.profiles.save(proposed).map_err(public_error)?;
        self.adopt(saved);
        Ok(())
    }

    fn adopt(&self, saved: PublishProfileSet) {
        let mut state = self.state.lock().expect("publish state mutex poisoned");
        state.profiles = saved;
        state.generation += 1;
    }

    fn set_status(&self, id: PublishProfileId, status: PublishStatus, message: Option<String>) {
        let mut state = self.state.lock().expect("publish state mutex poisoned");
        let runtime = state.runtimes.entry(id).or_default();
        runtime.status = status;
        runtime.message = message;
        state.generation += 1;
    }

    pub async fn save(
        &self,
        id: Option<PublishProfileId>,
        draft: PublishDraft,
        revision: &str,
        generation: &str,
    ) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let mut proposed = self.checked(revision, generation)?;
        let mut profile = match id {
            Some(id) => {
                self.require_stopped(id)?;
                Self::profile(&proposed, id)?
            }
            None => PublishProfile::new(draft.label.clone(), draft.target.clone()),
        };
        let mode_changed = profile.mode != draft.mode;
        if matches!(&draft.target, PublishTarget::LegacySession { .. })
            && (id.is_none() || profile.target != draft.target)
        {
            return Err("以前のチャット対象は保存済みの配信設定でのみ継続できます。プロジェクトまたは一時利用を選んでください。".into());
        }
        validate_target(&self.store, &draft.target, &self.protected_roots)
            .await
            .map_err(|_| {
                "公開対象が無効です。利用できるプロジェクトまたは一時利用を選んでください。"
                    .to_string()
            })?;
        self.checked(revision, generation)?;
        profile.label = draft.label.trim().to_string();
        profile.bind = draft.bind;
        profile.tls = draft.tls;
        profile.mode = draft.mode;
        profile.target = draft.target;
        profile.tools = draft.tools;
        profile.max_concurrent_calls = draft.max_concurrent_calls;
        profile.background = draft.background;
        profile.enabled = false;
        if mode_changed {
            profile.authentication = PublishAuthentication::Unpaired {};
        }
        profile.validate().map_err(public_error)?;
        let id = profile.id;
        if let Some(index) = proposed.profiles.iter().position(|value| value.id == id) {
            proposed.profiles[index] = profile;
        } else {
            proposed.profiles.push(profile);
        }
        if mode_changed {
            let (saved, ()) = self
                .profiles
                .update(proposed.revision, |_| {
                    self.credentials.revoke(id)?;
                    Ok((proposed.clone(), ()))
                })
                .map_err(public_error)?;
            self.adopt(saved);
        } else {
            self.persist(&proposed)?;
        }
        self.set_status(id, PublishStatus::Stopped, None);
        Ok(self.projection_now())
    }

    pub async fn delete(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let mut proposed = self.checked(revision, generation)?;
        Self::profile(&proposed, id)?;
        self.require_stopped(id)?;
        proposed.profiles.retain(|profile| profile.id != id);
        let (saved, ()) = self
            .profiles
            .update(proposed.revision, |_| {
                self.credentials.revoke(id)?;
                Ok((proposed.clone(), ()))
            })
            .map_err(public_error)?;
        self.adopt(saved);
        self.state
            .lock()
            .expect("publish state mutex poisoned")
            .runtimes
            .remove(&id);
        Ok(self.projection_now())
    }

    pub async fn issue_token(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<PublishTokenReceipt, String> {
        let _lane = self.commands.lock().await;
        let proposed = self.checked(revision, generation)?;
        Self::profile(&proposed, id)?;
        self.require_stopped(id)?;
        let (saved, token) = self
            .profiles
            .update(proposed.revision, |mut current| {
                let (credential_id, token) = self.credentials.issue(id)?;
                let profile = current
                    .profiles
                    .iter_mut()
                    .find(|value| value.id == id)
                    .ok_or(PublishError::UnknownProfile)?;
                profile.authentication = PublishAuthentication::LocalCredential { credential_id };
                profile.enabled = false;
                Ok((current, token))
            })
            .map_err(public_error)?;
        self.adopt(saved);
        self.set_status(id, PublishStatus::Stopped, None);
        Ok(PublishTokenReceipt {
            projection: self.projection_now(),
            profile_id: id,
            token,
        })
    }

    pub async fn start(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<PublishProjection, String> {
        self.start_with_config(id, revision, generation, self.config.clone())
            .await
    }

    pub async fn start_with_config(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
        config: ResolvedConfig,
    ) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let mut proposed = self.checked(revision, generation)?;
        self.require_stopped(id)?;
        if self
            .state
            .lock()
            .expect("publish state mutex poisoned")
            .hidden
        {
            return Err("配信開始はアプリの画面を開いて操作してください。".into());
        }
        let mut profile = Self::profile(&proposed, id)?;
        let PublishAuthentication::LocalCredential { credential_id } = profile.authentication
        else {
            return Err("接続トークンを発行してください。".into());
        };
        let digest = self
            .credentials
            .verifier(id, credential_id)
            .map_err(public_error)?;
        profile.enabled = true;
        profile.validate().map_err(public_error)?;
        *proposed
            .profiles
            .iter_mut()
            .find(|value| value.id == id)
            .expect("profile checked") = profile.clone();
        proposed.validate().map_err(public_error)?;
        self.set_status(
            id,
            PublishStatus::Starting,
            Some("公開対象を確認しています。".into()),
        );
        let mut protected_roots = self.protected_roots.clone();
        protected_roots.extend(
            proposed
                .profiles
                .iter()
                .filter_map(|profile| profile.tls.as_ref().map(|tls| tls.private_key_path.clone())),
        );
        let prepared: Result<Arc<dyn PublishToolDispatcher>, PublishCallError> = match profile.mode
        {
            PublishMode::ReadTools {} => PublishReadDispatcher::new(
                profile.clone(),
                self.store.clone(),
                config.clone(),
                protected_roots.clone(),
            )
            .await
            .map(|dispatcher| Arc::new(dispatcher) as Arc<dyn PublishToolDispatcher>),
            PublishMode::Agent { .. } => match &self.remote_jobs {
                Some(jobs) => {
                    jobs.dispatcher(profile.clone(), config, protected_roots)
                        .await
                }
                None => Err(PublishCallError::Unavailable),
            },
        };
        let dispatcher = match prepared {
            Ok(dispatcher) => dispatcher,
            Err(_) => {
                self.set_status(
                    id,
                    PublishStatus::Error,
                    Some("公開対象を確認できません。対象と設定を見直してください。".into()),
                );
                return Ok(self.projection_now());
            }
        };
        if !self.start_pending(id) {
            if let Some(jobs) = &self.remote_jobs {
                jobs.cancel_profile(id);
            }
            return Ok(self.projection_now());
        }
        if let Err(error) = self.persist(&proposed) {
            self.set_status(id, PublishStatus::Error, Some(error.clone()));
            return Err(error);
        }
        match PublishHttpServer::start_with_tls(
            profile.bind,
            digest,
            dispatcher,
            profile.max_concurrent_calls,
            profile.tls.as_ref(),
        )
        .await
        {
            Ok(server) => {
                let mut state = self.state.lock().expect("publish state mutex poisoned");
                let may_start = !state.closing
                    && state
                        .runtimes
                        .get(&id)
                        .is_some_and(|runtime| runtime.status == PublishStatus::Starting);
                let runtime = state.runtimes.entry(id).or_default();
                runtime.endpoint = Some(server.endpoint());
                runtime.observer = Some(server.observation());
                if !may_start {
                    server.cancel();
                }
                runtime.server = Some(server);
                runtime.status = if may_start {
                    PublishStatus::Running
                } else {
                    PublishStatus::Stopping
                };
                runtime.message = Some(
                    if may_start {
                        "認証したクライアントからの呼び出しを受け付けています。"
                    } else {
                        "配信を停止しています。"
                    }
                    .into(),
                );
                state.generation += 1;
            }
            Err(_) => self.set_status(
                id,
                PublishStatus::Error,
                Some("配信を開始できません。ポートが使用中でないか確認してください。".into()),
            ),
        }
        Ok(self.projection_now())
    }

    pub async fn create_certificate(
        &self,
        id: PublishProfileId,
        ip: IpAddr,
        revision: &str,
        generation: &str,
    ) -> Result<super::tls::PublishCertificateReceipt, String> {
        let _lane = self.commands.lock().await;
        let proposed = self.checked(revision, generation)?;
        Self::profile(&proposed, id)?;
        self.require_stopped(id)?;
        if ip.is_unspecified() || ip.is_multicast() {
            return Err("この端末の具体的なIPアドレスを選んでください。".into());
        }
        super::tls::create_certificate(&self.tls_directory, id, ip)
            .map_err(|_| "証明書を作成できません。設定フォルダーを確認してください。".to_string())
    }

    pub async fn certificate(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<super::tls::PublishCertificateReceipt, String> {
        let _lane = self.commands.lock().await;
        let document = self.checked(revision, generation)?;
        let profile = Self::profile(&document, id)?;
        let tls = profile
            .tls
            .ok_or_else(|| "この設定には証明書がありません。".to_string())?;
        super::tls::certificate_receipt(&tls).map_err(|_| {
            "保存した証明書を読み込めません。証明書の設定を確認してください。".to_string()
        })
    }

    fn start_pending(&self, id: PublishProfileId) -> bool {
        let state = self.state.lock().expect("publish state mutex poisoned");
        !state.closing
            && state
                .runtimes
                .get(&id)
                .is_some_and(|runtime| runtime.status == PublishStatus::Starting)
    }

    async fn stop_owned(&self, id: PublishProfileId) -> bool {
        if let Some(jobs) = &self.remote_jobs {
            jobs.cancel_profile(id);
        }
        let mut server = {
            let mut state = self.state.lock().expect("publish state mutex poisoned");
            let runtime = state.runtimes.entry(id).or_default();
            runtime.status = PublishStatus::Stopping;
            runtime.message = Some("新しい受付を止め、実行中の処理の終了を確認しています。".into());
            let server = runtime.server.take();
            state.generation += 1;
            server
        };
        let server_done = match server.as_mut() {
            Some(server) => {
                server.cancel();
                server.stop().await
            }
            None => true,
        };
        let jobs_done = match &self.remote_jobs {
            Some(jobs) => jobs.drain_profile(id, Duration::from_secs(5)).await,
            None => true,
        };
        let done = server_done && jobs_done;
        let mut state = self.state.lock().expect("publish state mutex poisoned");
        let runtime = state.runtimes.entry(id).or_default();
        if done {
            runtime.status = PublishStatus::Stopped;
            runtime.message = None;
            runtime.observer = None;
        } else {
            runtime.server = server;
            runtime.message =
                Some("処理の終了確認を続けています。停止を再度押して確認できます。".into());
        }
        state.generation += 1;
        done
    }

    pub async fn stop(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let mut proposed = self.checked(revision, generation)?;
        Self::profile(&proposed, id)?;
        self.stop_owned(id).await;
        proposed
            .profiles
            .iter_mut()
            .find(|value| value.id == id)
            .expect("profile checked")
            .enabled = false;
        self.persist(&proposed)?;
        Ok(self.projection_now())
    }

    pub async fn revoke_token(
        &self,
        id: PublishProfileId,
        revision: &str,
        generation: &str,
    ) -> Result<PublishProjection, String> {
        let _lane = self.commands.lock().await;
        let mut proposed = self.checked(revision, generation)?;
        Self::profile(&proposed, id)?;
        self.stop_owned(id).await;
        let profile = proposed
            .profiles
            .iter_mut()
            .find(|value| value.id == id)
            .expect("profile checked");
        profile.authentication = PublishAuthentication::Unpaired {};
        profile.enabled = false;
        let (saved, ()) = self
            .profiles
            .update(proposed.revision, |_| {
                self.credentials.revoke(id)?;
                Ok((proposed.clone(), ()))
            })
            .map_err(public_error)?;
        self.adopt(saved);
        Ok(self.projection_now())
    }

    pub fn window_shown(&self) {
        let mut state = self.state.lock().expect("publish state mutex poisoned");
        state.hidden = false;
        state.generation += 1;
    }

    /// Close admission synchronously, before the UI window disappears. This also
    /// marks an in-progress start so it cannot publish after a close/quit request.
    pub fn window_hide_requested(&self) -> Vec<PublishProfileId> {
        self.cancel_lifecycle(false)
    }

    pub fn begin_shutdown(&self) {
        self.cancel_lifecycle(true);
    }

    fn cancel_lifecycle(&self, exiting: bool) -> Vec<PublishProfileId> {
        let mut state = self.state.lock().expect("publish state mutex poisoned");
        state.hidden = true;
        state.closing |= exiting;
        let ids = state
            .profiles
            .profiles
            .iter()
            .filter(|profile| {
                exiting || profile.background == PublishBackgroundPolicy::StopWhenWindowCloses
            })
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        for id in &ids {
            if let Some(runtime) = state.runtimes.get_mut(id) {
                if matches!(
                    runtime.status,
                    PublishStatus::Starting | PublishStatus::Running | PublishStatus::Stopping
                ) {
                    if let Some(server) = &runtime.server {
                        server.cancel();
                    }
                    runtime.status = PublishStatus::Stopping;
                    runtime.message = Some("配信を停止しています。".into());
                }
            }
        }
        state.generation += 1;
        drop(state);
        if let Some(jobs) = &self.remote_jobs {
            for id in &ids {
                jobs.cancel_profile(*id);
            }
        }
        ids
    }

    pub async fn finish_window_hide(&self, ids: Vec<PublishProfileId>) {
        let _lane = self.commands.lock().await;
        for id in ids {
            let stopping = self
                .state
                .lock()
                .expect("publish state mutex poisoned")
                .runtimes
                .get(&id)
                .is_some_and(|runtime| runtime.status == PublishStatus::Stopping);
            if stopping {
                self.stop_owned(id).await;
            }
        }
    }

    pub async fn shutdown(&self) -> bool {
        self.begin_shutdown();
        let _lane = self.commands.lock().await;
        let ids = {
            let mut state = self.state.lock().expect("publish state mutex poisoned");
            state.closing = true;
            state
                .profiles
                .profiles
                .iter()
                .map(|profile| profile.id)
                .collect::<Vec<_>>()
        };
        let mut stopped = true;
        for id in ids {
            stopped &= self.stop_owned(id).await;
        }
        stopped
    }
}

fn parse_revision(value: &str) -> Result<u64, String> {
    let number = value
        .parse::<u64>()
        .map_err(|_| "配信設定の対象が不正です。".to_string())?;
    if number.to_string() != value {
        return Err("配信設定の対象が不正です。".into());
    }
    Ok(number)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

fn public_error(error: PublishError) -> String {
    match error {
        PublishError::Unpaired => "接続トークンが未設定、または失効しています。発行し直してください。",
        PublishError::StaleRevision | PublishError::StoreBusy => "別の操作で配信設定が変わりました。最新の状態を取得して確認してください。",
        PublishError::ToolUnavailable => "公開するツールを選び直してください。",
        PublishError::InvalidConfiguration(_) => "設定を確認してください。待受IP、ポート、公開対象・モード、同時実行上限が必要です。別端末からの接続にはTLSを設定してください。",
        _ => "配信設定を読み書きできません。設定フォルダーとファイルを確認してください。",
    }.into()
}
