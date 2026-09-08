use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::{DeviceError, DeviceGrant, DeviceNetworkService, DirectoryPeer, VerifiedGrant};
use crate::config::{
    McpConfig, McpServerConfig, McpToolRouteConfig, McpTransportKind, ResolvedConfig,
};
use crate::error::ToolError;
use crate::mcp::{McpClient, McpOperationResult};
use crate::protocol::TurnId;
use crate::remote_agent::store::StoredDeviceReference;
use crate::runtime::RunControl;
use crate::session::SessionId;
use crate::storage::StoreBundle;
use crate::tool::ToolEffectClass;

#[derive(Debug, Clone, Serialize)]
pub struct DeviceDelegationRow {
    pub reference_id: String,
    pub root_task_id: String,
    pub device_id: String,
    pub profile_id: String,
    pub device_path: Vec<String>,
    pub job_id: Option<String>,
    pub state: String,
    pub stop_status: String,
    pub can_stop: bool,
    pub result: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DeviceNetworkJobs {
    pub incoming: Vec<crate::remote_agent::RemoteJobRow>,
    pub outgoing: Vec<DeviceDelegationRow>,
}

pub(super) struct OutgoingOwner {
    store: StoreBundle,
    inbound: Mutex<HashMap<SessionId, (Ulid, VerifiedGrant)>>,
    watched: Mutex<HashSet<(SessionId, TurnId)>>,
    lane: tokio::sync::Mutex<()>,
}
impl OutgoingOwner {
    pub fn new(store: StoreBundle) -> Self {
        Self {
            store,
            inbound: Mutex::new(HashMap::new()),
            watched: Mutex::new(HashSet::new()),
            lane: tokio::sync::Mutex::new(()),
        }
    }
}
pub(crate) fn server_id(peer: &DirectoryPeer) -> String {
    let digest = Sha256::digest(format!("{}\n{}", peer.device_id, peer.profile_id));
    format!("hub-device-{:x}", digest)
}
fn input_request_hash(
    prompt: &str,
    inputs: &[crate::remote_agent::artifacts::RemoteInputFile],
) -> Result<String, DeviceError> {
    if inputs.is_empty() {
        return Ok(format!("{:x}", Sha256::digest(prompt.as_bytes())));
    }
    let bytes =
        serde_json::to_vec(&(prompt, inputs)).map_err(|_| DeviceError::InvalidConfiguration)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

impl DeviceNetworkService {
    pub fn cached_artifact_manifest(
        &self,
        reference_id: &str,
        version: &str,
    ) -> Result<crate::remote_agent::artifacts::ArtifactManifest, DeviceError> {
        let id = reference_id
            .parse::<Ulid>()
            .map_err(|_| DeviceError::InvalidConfiguration)?;
        let store = self.inner.outgoing.store.remote_job_store();
        let row = store
            .device_reference(id)
            .map_err(|_| DeviceError::Storage)?
            .ok_or(DeviceError::ArtifactsUnavailable)?;
        if !terminal(&row.state) {
            return Err(DeviceError::ArtifactsUnavailable);
        }
        let job = row
            .job_id
            .as_deref()
            .ok_or(DeviceError::ArtifactsUnavailable)?;
        store
            .cached_artifacts(id, job, Some(version))
            .map_err(|_| DeviceError::ArtifactsUnavailable)?
            .map(|bundle| bundle.manifest)
            .ok_or(DeviceError::ArtifactsUnavailable)
    }
    /// Fetch exactly one settled job version into the app-owned immutable cache.
    /// A cached version remains available when the peer is OFF or its Hub authority retires.
    pub async fn artifacts(
        &self,
        reference_id: &str,
        version: Option<&str>,
    ) -> Result<crate::remote_agent::artifacts::ArtifactManifest, DeviceError> {
        let id = reference_id
            .parse::<Ulid>()
            .map_err(|_| DeviceError::InvalidConfiguration)?;
        let _lane = self.inner.outgoing.lane.lock().await;
        let store = self.inner.outgoing.store.remote_job_store();
        let row = store
            .device_reference(id)
            .map_err(|_| DeviceError::Storage)?
            .ok_or(DeviceError::ArtifactsUnavailable)?;
        if !terminal(&row.state) {
            return Err(DeviceError::ArtifactsUnavailable);
        }
        let job = row
            .job_id
            .as_deref()
            .ok_or(DeviceError::ArtifactsUnavailable)?;
        if let Some(bundle) = store
            .cached_artifacts(id, job, version)
            .map_err(|_| DeviceError::ArtifactsUnavailable)?
        {
            return Ok(bundle.manifest);
        }
        let client = self.client()?;
        let http = client.http();
        let _identity = http.acquire().await;
        let grant = client
            .grant(
                &row.peer,
                &row.root_task_id,
                &row.request_key,
                row.parent_grant_id
                    .as_deref()
                    .zip(row.parent_job_id.as_deref()),
                "observe",
            )
            .await?;
        self.validate_grant(&grant, &row.peer, &row.root_task_id, &row.request_key)?;
        if row
            .claims
            .as_ref()
            .is_some_and(|claims| claims != &grant.claims)
        {
            return Err(DeviceError::GrantDenied);
        }
        let operation = self
            .peer_operation(
                &grant,
                Some("task_artifacts"),
                json!({"job_id":job,"version":version}),
                || Ok(()),
            )
            .await
            .map_err(|_| DeviceError::ArtifactsUnavailable)?;
        let McpOperationResult::ToolCalled { raw_result, .. } = operation else {
            return Err(DeviceError::InvalidResponse);
        };
        if raw_result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(DeviceError::ArtifactsUnavailable);
        }
        let data = raw_result
            .get("structuredContent")
            .cloned()
            .or_else(|| {
                raw_result
                    .get("content")?
                    .as_array()?
                    .iter()
                    .find_map(|part| {
                        serde_json::from_str::<Value>(part.get("text")?.as_str()?).ok()
                    })
            })
            .ok_or(DeviceError::InvalidResponse)?;
        let bundle: crate::remote_agent::artifacts::ArtifactBundle =
            serde_json::from_value(data).map_err(|_| DeviceError::InvalidResponse)?;
        bundle
            .validate()
            .map_err(|_| DeviceError::InvalidResponse)?;
        if bundle.manifest.job_id != job || version.is_some_and(|v| v != bundle.manifest.version) {
            return Err(DeviceError::InvalidResponse);
        }
        store
            .cache_artifacts(id, &bundle)
            .map_err(|_| DeviceError::Storage)?;
        Ok(bundle.manifest)
    }

    /// Export only the already-reviewed cache version. This method performs no network I/O.
    pub fn export_artifacts(
        &self,
        reference_id: &str,
        version: &str,
        new_directory: &camino::Utf8Path,
    ) -> Result<(), String> {
        let id = reference_id
            .parse::<Ulid>()
            .map_err(|_| DeviceError::InvalidConfiguration.to_string())?;
        let store = self.inner.outgoing.store.remote_job_store();
        let row = store
            .device_reference(id)
            .map_err(|_| DeviceError::Storage.to_string())?
            .ok_or_else(|| DeviceError::ArtifactsUnavailable.to_string())?;
        let job = row
            .job_id
            .as_deref()
            .ok_or_else(|| DeviceError::ArtifactsUnavailable.to_string())?;
        if !terminal(&row.state) {
            return Err(DeviceError::ArtifactsUnavailable.to_string());
        }
        let bundle = store
            .cached_artifacts(id, job, Some(version))
            .map_err(|_| DeviceError::ArtifactsUnavailable.to_string())?
            .ok_or_else(|| DeviceError::ArtifactsUnavailable.to_string())?;
        crate::remote_agent::artifacts::export_bundle(&bundle, new_directory)
            .map_err(|error| error.to_string())
    }
}
fn server_config(peer: &DirectoryPeer) -> McpServerConfig {
    McpServerConfig {
        display_name: Some(format!("{} ({})", peer.label, peer.device_id)),
        id: server_id(peer),
        enabled: true,
        transport: McpTransportKind::Http,
        base_url: peer.endpoint.clone(),
        timeout_ms: 30000,
        tool_routes: vec![
            McpToolRouteConfig {
                name: "task_artifacts".into(),
                effect: ToolEffectClass::Read,
            },
            McpToolRouteConfig {
                name: "delegate_task".into(),
                effect: ToolEffectClass::Destructive,
            },
            McpToolRouteConfig {
                name: "task_status".into(),
                effect: ToolEffectClass::Read,
            },
            McpToolRouteConfig {
                name: "cancel_task".into(),
                effect: ToolEffectClass::Destructive,
            },
        ],
        headers: Default::default(),
        remote_agent: true,
        trusted_certificate_pem: None,
    }
}
fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "interrupted")
}
fn public_row(row: StoredDeviceReference) -> DeviceDelegationRow {
    DeviceDelegationRow {
        reference_id: row.id.to_string(),
        root_task_id: row.root_task_id,
        device_id: row.device_id,
        profile_id: row.profile_id,
        device_path: row
            .claims
            .as_ref()
            .map(|claims| claims.device_path.clone())
            .unwrap_or_default(),
        job_id: row.job_id,
        can_stop: !terminal(&row.state) && row.stop_status != "requested",
        state: row.state,
        stop_status: row.stop_status,
        result: row.result,
    }
}
fn safe_error(error: DeviceError) -> ToolError {
    ToolError::Message(format!("device delegation: {error}"))
}

impl DeviceNetworkService {
    pub(crate) fn capture_inbound(&self, session: SessionId, job: Ulid, grant: VerifiedGrant) {
        let mut inbound = self.inner.outgoing.inbound.lock().unwrap();
        // At most 16 workers execute. Keep ancestry for their completed descendants in this process.
        if inbound.len() < 4096 {
            inbound.insert(session, (job, grant));
        }
    }
    async fn selected_directory(
        &self,
        session: SessionId,
    ) -> Result<Vec<DirectoryPeer>, DeviceError> {
        let parent = self
            .inner
            .outgoing
            .inbound
            .lock()
            .unwrap()
            .get(&session)
            .map(|(_, grant)| grant.grant_id.clone());
        let client = self.client()?;
        let directory = client.directory(parent.as_deref()).await?;
        let state = self.inner.state.lock().unwrap();
        if state.settings.hub_id.as_ref() != Some(&directory.hub_id) {
            return Err(DeviceError::InvalidResponse);
        }
        Ok(directory
            .peers
            .into_iter()
            .filter(|peer| {
                peer.mode == "agent"
                    && state.settings.selected_peers.iter().any(|selected| {
                        selected.device_id == peer.device_id
                            && selected.profile_id == peer.profile_id
                    })
            })
            .collect())
    }
    pub(crate) async fn augment_runtime_config(
        &self,
        config: &mut ResolvedConfig,
        session: SessionId,
    ) {
        // No credentials or private TLS material enter even this ephemeral model configuration.
        if self
            .inner
            .store
            .remote_job_store()
            .job_id_for_session(session)
            .ok()
            .flatten()
            .is_some()
            && !self
                .inner
                .outgoing
                .inbound
                .lock()
                .unwrap()
                .contains_key(&session)
        {
            return;
        }
        let Ok(peers) = self.selected_directory(session).await else {
            return;
        };
        for peer in peers {
            let server = server_config(&peer);
            if !config.mcp.servers.iter().any(|old| old.id == server.id) {
                config.mcp.servers.push(server);
            }
        }
        if config.mcp.servers.iter().any(|server| server.enabled) {
            config.mcp.enabled = true;
        }
    }
    pub(crate) fn owns_server(&self, id: &str) -> bool {
        id.starts_with("hub-device-")
    }

    pub(super) fn peer_http(&self, peer: &DirectoryPeer) -> Result<reqwest::Client, DeviceError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        let identity = state
            .identity
            .as_ref()
            .ok_or(DeviceError::InvalidIdentity)?;
        crate::mcp_publish::tls::managed_peer_http(
            state
                .settings
                .certificate_pem
                .as_deref()
                .ok_or(DeviceError::InvalidIdentity)?,
            identity.private_key_pem(),
            &state.shared.ca_certificate_pem,
            &peer.certificate_pem,
        )
        .map_err(|_| DeviceError::InvalidIdentity)
    }
    pub(super) async fn peer_operation(
        &self,
        grant: &DeviceGrant,
        name: Option<&str>,
        arguments: Value,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        let mut server = server_config(&grant.peer);
        let id = server.id.clone();
        server
            .headers
            .insert("Authorization".into(), format!("Bearer {}", grant.token));
        let client = McpClient::new(McpConfig {
            enabled: true,
            servers: vec![server],
        })
        .with_runtime_http(&id, self.peer_http(&grant.peer).map_err(safe_error)?);
        match name {
            Some(name) => {
                client
                    .call_tool(&id, name, arguments, &mut checkpoint)
                    .await
            }
            None => client.list_tools(&id, &mut checkpoint).await,
        }
    }
    fn validate_grant(
        &self,
        grant: &DeviceGrant,
        peer: &DirectoryPeer,
        root: &str,
        key: &str,
    ) -> Result<(), DeviceError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        let claims = &grant.claims;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if state.settings.hub_id.as_ref() != Some(&claims.hub_id)
            || state.settings.device_id.as_ref() != Some(&claims.actor_device_id)
            || claims.audience_device_id != peer.device_id
            || claims.profile_id != peer.profile_id
            || claims.root_task_id != root
            || claims.request_key != key
            || grant.peer.device_id != peer.device_id
            || grant.peer.profile_id != peer.profile_id
            || grant.peer.scope_id != claims.scope_id
            || claims.mode != "agent"
            || grant.peer.mode != "agent"
            || grant.token.len() > 256
            || grant.token.len() < 32
            || grant.peer.endpoint.len() > 2048
            || grant.peer.certificate_pem.len() > 65536
            || !super::stable_id(&grant.grant_id)
            || grant.expires_at_ms <= now
        {
            return Err(DeviceError::InvalidResponse);
        }
        Ok(())
    }
    pub(crate) async fn execute_operation(
        &self,
        session: SessionId,
        turn: TurnId,
        control: RunControl,
        id: &str,
        name: Option<&str>,
        arguments: Value,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        if !self.owns_server(id) {
            return Err(safe_error(DeviceError::PolicyDenied));
        }
        if name == Some("task_artifacts") {
            let job = arguments
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| safe_error(DeviceError::InvalidConfiguration))?;
            let row = self
                .inner
                .outgoing
                .store
                .remote_job_store()
                .find_device_reference(session, None, Some(job))
                .map_err(|_| safe_error(DeviceError::Storage))?
                .filter(|row| server_id(&row.peer) == id)
                .ok_or_else(|| safe_error(DeviceError::GrantDenied))?;
            let version = arguments
                .get("version")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| safe_error(DeviceError::InvalidConfiguration))
                })
                .transpose()?;
            checkpoint()?;
            let manifest = self
                .artifacts(&row.id.to_string(), version)
                .await
                .map_err(safe_error)?;
            checkpoint()?;
            let bundle = self
                .inner
                .outgoing
                .store
                .remote_job_store()
                .cached_artifacts(row.id, job, Some(&manifest.version))
                .map_err(|_| safe_error(DeviceError::Storage))?
                .ok_or_else(|| safe_error(DeviceError::ArtifactsUnavailable))?;
            let data = serde_json::to_value(bundle)
                .map_err(|_| safe_error(DeviceError::InvalidResponse))?;
            return Ok(McpOperationResult::ToolCalled {
                server_id: id.into(),
                endpoint: row.peer.endpoint,
                tool_name: "task_artifacts".into(),
                output_text: serde_json::to_string(&data)
                    .map_err(|_| safe_error(DeviceError::InvalidResponse))?,
                raw_result: json!({"structuredContent":data,"content":[],"isError":false}),
            });
        }
        if matches!(name, Some("task_status" | "cancel_task")) {
            let _lane = self.inner.outgoing.lane.lock().await;
            let key = arguments
                .get("request_key")
                .and_then(Value::as_str)
                .map(|key| canonical_key(session, turn, key));
            let job = arguments.get("job_id").and_then(Value::as_str);
            let row = self
                .inner
                .outgoing
                .store
                .remote_job_store()
                .find_device_reference(session, key.as_deref(), job)
                .map_err(|_| safe_error(DeviceError::Storage))?
                .filter(|row| server_id(&row.peer) == id)
                .ok_or_else(|| safe_error(DeviceError::GrantDenied))?;
            checkpoint()?;
            return self.control_reference(row, name.unwrap(), checkpoint).await;
        }
        if name.is_some_and(|name| name != "delegate_task") {
            return Err(safe_error(DeviceError::PolicyDenied));
        }
        let peer = self
            .selected_directory(session)
            .await
            .map_err(safe_error)?
            .into_iter()
            .find(|peer| server_id(peer) == id)
            .ok_or_else(|| safe_error(DeviceError::PolicyDenied))?;
        let inbound = self
            .inner
            .outgoing
            .inbound
            .lock()
            .unwrap()
            .get(&session)
            .cloned();
        let root = inbound
            .as_ref()
            .map(|(_, grant)| grant.claims.root_task_id.clone())
            .unwrap_or_else(|| turn.to_string());
        let parent_grant = inbound.as_ref().map(|(_, grant)| grant.grant_id.clone());
        let parent_job = inbound.as_ref().map(|(job, _)| job.to_string());
        let client = self.client().map_err(safe_error)?;
        // A grant and its MCP request must use the same actor certificate. Renewal
        // may happen between operations, never between grant issuance and dispatch.
        let http = client.http();
        let _identity = http.acquire().await;
        if name.is_none() {
            let grant = client
                .inspect_peer(
                    &peer.device_id,
                    &peer.profile_id,
                    parent_grant.as_deref().zip(parent_job.as_deref()),
                )
                .await
                .map_err(safe_error)?;
            self.validate_grant(
                &grant,
                &peer,
                &grant.claims.root_task_id,
                &grant.claims.request_key,
            )
            .map_err(safe_error)?;
            return self
                .peer_operation(&grant, None, json!({}), checkpoint)
                .await;
        }
        let object = arguments
            .as_object()
            .ok_or_else(|| safe_error(DeviceError::InvalidConfiguration))?;
        let raw_key = object
            .get("request_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty() && key.len() <= 128)
            .ok_or_else(|| safe_error(DeviceError::InvalidConfiguration))?;
        let prompt = object
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.trim().is_empty() && prompt.len() <= 32768)
            .ok_or_else(|| safe_error(DeviceError::InvalidConfiguration))?;
        let key = canonical_key(session, turn, raw_key);
        let inputs: Vec<crate::remote_agent::artifacts::RemoteInputFile> = object
            .get("inputs")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| safe_error(DeviceError::InvalidConfiguration))?
            .unwrap_or_default();
        crate::remote_agent::artifacts::validate_inputs(&inputs)
            .map_err(|_| safe_error(DeviceError::InvalidConfiguration))?;
        let prompt_hash = input_request_hash(prompt, &inputs).map_err(safe_error)?;
        let _lane = self.inner.outgoing.lane.lock().await;
        let proposed = StoredDeviceReference {
            id: Ulid::new(),
            session_id: session,
            turn_id: turn,
            device_id: peer.device_id.clone(),
            profile_id: peer.profile_id.clone(),
            root_task_id: root.clone(),
            request_key: key.clone(),
            prompt_hash,
            parent_grant_id: parent_grant,
            parent_job_id: parent_job,
            peer: peer.clone(),
            claims: None,
            job_id: None,
            state: "preparing".into(),
            stop_status: "none".into(),
            result: None,
        };
        checkpoint()?;
        let store = self.inner.outgoing.store.remote_job_store();
        let recent = store
            .device_references(None, 64)
            .map_err(|_| safe_error(DeviceError::Storage))?;
        let existing = recent.iter().any(|row| {
            row.session_id == session
                && row.turn_id == turn
                && row.device_id == peer.device_id
                && row.profile_id == peer.profile_id
                && row.request_key == key
        });
        if !existing && recent.iter().filter(|row| !terminal(&row.state)).count() >= 64 {
            return Err(safe_error(DeviceError::ReceiverBusy));
        }
        let mut row = store
            .accept_device_reference(&proposed)
            .map_err(|_| safe_error(DeviceError::Storage))?;
        if row.id != proposed.id && row.state != "preparing" {
            return self.control_reference(row, "task_status", checkpoint).await;
        }
        let grant_result = client
            .grant(
                &peer,
                &root,
                &key,
                row.parent_grant_id
                    .as_deref()
                    .zip(row.parent_job_id.as_deref()),
                "execute",
            )
            .await;
        let grant = match grant_result {
            Ok(grant) => grant,
            Err(error) => {
                // No request body has reached a receiver. This is a failed admission,
                // not an unresolved remote execution consuming the active-reference budget.
                row.state = "failed".into();
                row.result = Some(format!("委任を開始できませんでした: {error}"));
                store
                    .update_device_reference(&row)
                    .map_err(|_| safe_error(DeviceError::Storage))?;
                return Err(safe_error(error));
            }
        };
        self.validate_grant(&grant, &peer, &root, &key)
            .map_err(safe_error)?;
        row.peer = grant.peer.clone();
        row.claims = Some(grant.claims.clone());
        row.state = "unknown".into();
        store
            .update_device_reference(&row)
            .map_err(|_| safe_error(DeviceError::Storage))?;
        self.watch_control(session, turn, control);
        let mut args = json!({"request_key":key,"prompt":prompt,"parent":{"peer_id":grant.claims.actor_device_id,"task_id":root,"turn_id":turn.to_string()}});
        if !inputs.is_empty() {
            args["inputs"] = serde_json::to_value(&inputs)
                .map_err(|_| safe_error(DeviceError::InvalidConfiguration))?;
        }
        let operation = self
            .peer_operation(&grant, Some("delegate_task"), args, checkpoint)
            .await;
        match &operation {
            Ok(operation) => self
                .accept_remote_result(&mut row, operation)
                .map_err(safe_error)?,
            Err(_) => {
                row.state = "unknown".into();
                store
                    .update_device_reference(&row)
                    .map_err(|_| safe_error(DeviceError::Storage))?;
            }
        }
        operation
    }

    fn accept_remote_result(
        &self,
        row: &mut StoredDeviceReference,
        operation: &McpOperationResult,
    ) -> Result<(), DeviceError> {
        let McpOperationResult::ToolCalled { raw_result, .. } = operation else {
            return Err(DeviceError::InvalidResponse);
        };
        if raw_result.get("isError").and_then(Value::as_bool) == Some(true) {
            if !terminal(&row.state) {
                row.state = "unknown".into();
            }
        } else {
            let data = raw_result
                .get("structuredContent")
                .cloned()
                .or_else(|| {
                    raw_result
                        .get("content")?
                        .as_array()?
                        .iter()
                        .find_map(|part| {
                            serde_json::from_str::<Value>(part.get("text")?.as_str()?).ok()
                        })
                })
                .ok_or(DeviceError::InvalidResponse)?;
            let job = data
                .get("job_id")
                .and_then(Value::as_str)
                .filter(|id| id.parse::<Ulid>().is_ok())
                .ok_or(DeviceError::InvalidResponse)?;
            if row.job_id.as_ref().is_some_and(|old| old != job)
                || data.get("profile_id").and_then(Value::as_str) != Some(row.profile_id.as_str())
            {
                return Err(DeviceError::InvalidResponse);
            }
            let state = data
                .get("state")
                .and_then(Value::as_str)
                .filter(|state| {
                    matches!(
                        *state,
                        "accepted"
                            | "running"
                            | "awaiting_approval"
                            | "cancelling"
                            | "completed"
                            | "failed"
                            | "interrupted"
                    )
                })
                .ok_or(DeviceError::InvalidResponse)?;
            if !terminal(&row.state) {
                row.state = state.into();
            }
            row.job_id = Some(job.into());
            row.result = data
                .get("result")
                .and_then(Value::as_str)
                .map(|text| text.chars().take(65536).collect());
            if row.stop_status != "none" {
                row.stop_status = if terminal(state) {
                    "confirmed"
                } else {
                    "requested"
                }
                .into();
            }
        }
        self.inner
            .outgoing
            .store
            .remote_job_store()
            .update_device_reference(row)
            .map_err(|_| DeviceError::Storage)
    }

    async fn control_reference(
        &self,
        row: StoredDeviceReference,
        tool: &str,
        checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        let id = row.id;
        let result = self.control_reference_inner(row, tool, checkpoint).await;
        if result.is_err() && tool == "cancel_task" {
            let store = self.inner.outgoing.store.remote_job_store();
            if let Some(mut current) = store
                .device_reference(id)
                .map_err(|_| safe_error(DeviceError::Storage))?
            {
                if !terminal(&current.state) {
                    current.stop_status = "unconfirmed".into();
                    store
                        .update_device_reference(&current)
                        .map_err(|_| safe_error(DeviceError::Storage))?;
                }
            }
        }
        result
    }
    async fn control_reference_inner(
        &self,
        mut row: StoredDeviceReference,
        tool: &str,
        mut checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<McpOperationResult, ToolError> {
        let action = if tool == "cancel_task" {
            "cancel"
        } else {
            "observe"
        };
        if tool == "cancel_task" {
            row.stop_status = "requested".into();
            self.inner
                .outgoing
                .store
                .remote_job_store()
                .update_device_reference(&row)
                .map_err(|_| safe_error(DeviceError::Storage))?;
        }
        let client = {
            self.inner
                .state
                .lock()
                .map_err(|_| safe_error(DeviceError::Unavailable))?
                .client
                .clone()
                .ok_or_else(|| safe_error(DeviceError::Unavailable))?
        };
        // Existing references use their immutable audience and lineage, even after directory removal.
        let http = client.http();
        let _identity = http.acquire().await;
        let grant = match client
            .grant(
                &row.peer,
                &row.root_task_id,
                &row.request_key,
                row.parent_grant_id
                    .as_deref()
                    .zip(row.parent_job_id.as_deref()),
                action,
            )
            .await
        {
            Ok(grant) => grant,
            Err(error) => {
                if row.stop_status != "none" {
                    row.stop_status = "unconfirmed".into();
                }
                self.inner
                    .outgoing
                    .store
                    .remote_job_store()
                    .update_device_reference(&row)
                    .map_err(|_| safe_error(DeviceError::Storage))?;
                return Err(safe_error(error));
            }
        };
        self.validate_grant(&grant, &row.peer, &row.root_task_id, &row.request_key)
            .map_err(safe_error)?;
        if row
            .claims
            .as_ref()
            .is_some_and(|claims| claims != &grant.claims)
        {
            return Err(safe_error(DeviceError::GrantDenied));
        }
        row.peer = grant.peer.clone();
        row.claims = Some(grant.claims.clone());
        if tool == "cancel_task" && row.job_id.is_none() {
            match self
                .peer_operation(
                    &grant,
                    Some("task_status"),
                    json!({"request_key":row.request_key}),
                    &mut checkpoint,
                )
                .await
            {
                Ok(operation) => self
                    .accept_remote_result(&mut row, &operation)
                    .map_err(safe_error)?,
                Err(error) => {
                    row.stop_status = "unconfirmed".into();
                    self.inner
                        .outgoing
                        .store
                        .remote_job_store()
                        .update_device_reference(&row)
                        .map_err(|_| safe_error(DeviceError::Storage))?;
                    return Err(error);
                }
            }
        }
        let args = match &row.job_id {
            Some(job) => json!({"job_id":job}),
            None if tool == "task_status" => json!({"request_key":row.request_key}),
            None => return Err(safe_error(DeviceError::Unavailable)),
        };
        let result = self
            .peer_operation(&grant, Some(tool), args, checkpoint)
            .await;
        match &result {
            Ok(operation) => self
                .accept_remote_result(&mut row, operation)
                .map_err(safe_error)?,
            Err(_) => {
                if row.stop_status != "none" {
                    row.stop_status = "unconfirmed".into();
                }
                self.inner
                    .outgoing
                    .store
                    .remote_job_store()
                    .update_device_reference(&row)
                    .map_err(|_| safe_error(DeviceError::Storage))?;
            }
        }
        result
    }
    fn watch_control(&self, session: SessionId, turn: TurnId, control: RunControl) {
        if !self
            .inner
            .outgoing
            .watched
            .lock()
            .unwrap()
            .insert((session, turn))
        {
            return;
        }
        let weak = self.downgrade();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=control.token().cancelled_owned()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(10))=>{}}
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if control.cause().is_some() {
                    service.cancel_session_outgoing(session, Some(turn)).await;
                    service
                        .inner
                        .outgoing
                        .watched
                        .lock()
                        .unwrap()
                        .remove(&(session, turn));
                    break;
                }
                let active = service
                    .inner
                    .outgoing
                    .store
                    .remote_job_store()
                    .device_references(Some(session), 64)
                    .is_ok_and(|rows| {
                        rows.iter()
                            .any(|row| row.turn_id == turn && !terminal(&row.state))
                    });
                if !active {
                    service
                        .inner
                        .outgoing
                        .watched
                        .lock()
                        .unwrap()
                        .remove(&(session, turn));
                    break;
                }
            }
        });
    }
    pub async fn jobs(&self) -> Result<DeviceNetworkJobs, DeviceError> {
        let profile = self
            .inner
            .state
            .lock()
            .unwrap()
            .settings
            .receiver
            .profile_id;
        let incoming = self
            .inner
            .jobs
            .rows_all()
            .await
            .map_err(|_| DeviceError::Storage)?
            .into_iter()
            .filter(|row| row.profile_id == profile && row.network.is_some())
            .collect();
        let outgoing = self
            .inner
            .outgoing
            .store
            .remote_job_store()
            .device_references(None, 64)
            .map_err(|_| DeviceError::Storage)?
            .into_iter()
            .map(public_row)
            .collect();
        Ok(DeviceNetworkJobs { incoming, outgoing })
    }
    pub async fn cancel(&self, reference_id: &str) -> Result<DeviceDelegationRow, DeviceError> {
        let id = reference_id
            .parse::<Ulid>()
            .map_err(|_| DeviceError::InvalidConfiguration)?;
        let _lane = self.inner.outgoing.lane.lock().await;
        let store = self.inner.outgoing.store.remote_job_store();
        let row = store
            .device_reference(id)
            .map_err(|_| DeviceError::Storage)?
            .ok_or(DeviceError::InvalidConfiguration)?;
        if !terminal(&row.state) {
            let _ = self.control_reference(row, "cancel_task", || Ok(())).await;
        }
        store
            .device_reference(id)
            .map_err(|_| DeviceError::Storage)?
            .map(public_row)
            .ok_or(DeviceError::Storage)
    }
    pub(super) async fn poll_outgoing(&self) {
        let Ok(_lane) = self.inner.outgoing.lane.try_lock() else {
            return;
        };
        let Ok(rows) = self
            .inner
            .outgoing
            .store
            .remote_job_store()
            .device_references(None, 64)
        else {
            return;
        };
        for row in rows.into_iter().filter(|row| !terminal(&row.state)) {
            let _ = self.control_reference(row, "task_status", || Ok(())).await;
        }
    }
    async fn cancel_session_outgoing(&self, session: SessionId, turn: Option<TurnId>) {
        let Ok(rows) = self
            .inner
            .outgoing
            .store
            .remote_job_store()
            .device_references(Some(session), 64)
        else {
            return;
        };
        let client = {
            self.inner
                .state
                .lock()
                .ok()
                .and_then(|state| state.client.clone())
        };
        if let Some(client) = client {
            let roots: HashSet<_> = rows
                .iter()
                .filter(|row| {
                    !terminal(&row.state)
                        && turn.is_none_or(|turn| row.turn_id == turn)
                        && row
                            .claims
                            .as_ref()
                            .is_some_and(|claims| claims.origin_device_id == client.device_id)
                })
                .map(|row| row.root_task_id.clone())
                .collect();
            for root in roots {
                let _ = client.cancel_lineage(&root).await;
            }
        }
        for row in rows
            .into_iter()
            .filter(|row| !terminal(&row.state) && turn.is_none_or(|turn| row.turn_id == turn))
        {
            let _ = self.cancel(&row.id.to_string()).await;
        }
    }
    pub(super) async fn cancel_all_outgoing(&self) {
        self.finish_outgoing_shutdown(
            std::time::Duration::from_secs(10),
            self.request_all_outgoing_stops(),
        )
        .await;
    }
    async fn finish_outgoing_shutdown(
        &self,
        deadline: std::time::Duration,
        work: impl std::future::Future<Output = ()>,
    ) -> bool {
        let completed = tokio::time::timeout(deadline, work).await.is_ok();
        if !completed {
            let _ = self
                .inner
                .outgoing
                .store
                .remote_job_store()
                .mark_device_cancellations_unconfirmed();
        }
        completed
    }
    async fn request_all_outgoing_stops(&self) {
        let Ok(rows) = self
            .inner
            .outgoing
            .store
            .remote_job_store()
            .device_references(None, 64)
        else {
            return;
        };
        for row in rows.into_iter().filter(|row| !terminal(&row.state)) {
            let _ = self.cancel(&row.id.to_string()).await;
        }
    }
}
pub(crate) fn canonical_key(session: SessionId, turn: TurnId, key: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{session}\n{turn}\n{key}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStore, StoragePaths};
    #[tokio::test]
    async fn shutdown_deadline_releases_command_lane_and_keeps_unconfirmed_remote_work() {
        let temp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let paths = StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let network = DeviceNetworkService::for_workspace(
            root.join("config/device"),
            workspace,
            StoreBundle::new(sqlite),
            ResolvedConfig::default(),
        )
        .await
        .unwrap();
        let store = network.inner.store.remote_job_store();
        let peer = DirectoryPeer {
            device_id: "worker-b".into(),
            profile_id: "profile-b".into(),
            label: "WinB".into(),
            name: "receiver".into(),
            endpoint: "https://127.0.0.1:7332/mcp".into(),
            mode: "agent".into(),
            scope_id: "scope".into(),
            certificate_pem: "public-test-certificate".into(),
            certificate_sha256: "a".repeat(64),
        };
        let pending = StoredDeviceReference {
            id: Ulid::new(),
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            device_id: peer.device_id.clone(),
            profile_id: peer.profile_id.clone(),
            root_task_id: "root".into(),
            request_key: "request".into(),
            prompt_hash: "b".repeat(64),
            parent_grant_id: None,
            parent_job_id: None,
            peer,
            claims: None,
            job_id: Some(Ulid::new().to_string()),
            state: "running".into(),
            stop_status: "none".into(),
            result: None,
        };
        let pending = store.accept_device_reference(&pending).unwrap();
        let mut completed = pending.clone();
        completed.id = Ulid::new();
        completed.job_id = Some(Ulid::new().to_string());
        completed.request_key = "completed".into();
        completed.state = "completed".into();
        completed.result = Some("received result".into());
        let completed = store.accept_device_reference(&completed).unwrap();
        network.begin_shutdown();
        let drained = network
            .finish_outgoing_shutdown(std::time::Duration::from_millis(20), async {
                let _guard = network.inner.outgoing.lane.lock().await;
                std::future::pending::<()>().await;
            })
            .await;
        assert!(!drained);
        assert!(
            network.inner.outgoing.lane.try_lock().is_ok(),
            "deadline must drop the pending operation's command-lane guard"
        );
        let after = store.device_reference(pending.id).unwrap().unwrap();
        assert_eq!(after.state, "running");
        assert_eq!(after.stop_status, "unconfirmed");
        let done = store.device_reference(completed.id).unwrap().unwrap();
        assert_eq!(done.state, "completed");
        assert_eq!(done.stop_status, "none");
        assert_eq!(done.result.as_deref(), Some("received result"));
        store.update_device_reference(&pending).unwrap();
        assert_eq!(
            store
                .device_reference(pending.id)
                .unwrap()
                .unwrap()
                .stop_status,
            "unconfirmed",
            "a late pre-shutdown observation cannot erase the stop request"
        );
    }
}
