//! Agent-facing shared work uses the same mTLS and Hub authority as Desktop.
//! It has no navigation state: model calls must not select a project or replace a draft.
use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};

const MAX_AGENT_ARTIFACT_TEXT: u64 = 64 * 1024;
const MAX_AGENT_ARTIFACT_FILE: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct AgentDownload {
    asset: WorkAsset,
    content_base64: String,
}
#[derive(Deserialize)]
struct SharedChildArtifacts {
    items: Vec<WorkAsset>,
}

#[derive(Deserialize, Serialize)]
struct AgentEnvironment {
    id: String,
    label: String,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    device_label: Option<String>,
    enabled: bool,
    #[serde(default)]
    can_submit: bool,
    capacity: u32,
    occupied: u32,
    #[serde(default)]
    capabilities: Vec<Value>,
    #[serde(default)]
    runner_contact: Option<WorkRunnerContact>,
}

#[derive(Deserialize)]
struct AgentOriginTurnAdmission {
    origin_session_ref: String,
    origin_turn_ref: String,
    epoch: u64,
}

#[derive(Deserialize)]
struct AgentOriginTurnStop {
    accepted: bool,
    origin_session_ref: String,
    origin_turn_ref: Option<String>,
    request_id: String,
}

impl DeviceNetworkService {
    fn configured_origin_hub_binding(&self) -> Option<String> {
        let state = self.inner.state.lock().unwrap();
        let hub_id = state.settings.hub_id.as_deref()?;
        if state.shared.ca_certificate_pem.is_empty() {
            return None;
        }
        Some(format!(
            "{}|{:x}",
            hub_id,
            Sha256::digest(state.shared.ca_certificate_pem.as_bytes())
        ))
    }

    /// Save an exact remote Stop before attempting network I/O. This queue is
    /// independent of an uncertain job submission, which Stop must be able to fence.
    #[cfg(test)]
    pub(crate) fn queue_origin_turn_stop(
        &self,
        origin_session_ref: &str,
        origin_turn_ref: &str,
        origin_turn_revision: u64,
    ) -> Result<bool, String> {
        self.queue_origin_turn_stop_as(
            origin_session_ref,
            origin_turn_ref,
            origin_turn_revision,
            false,
        )
    }

    pub(crate) fn prepare_origin_turn_stop(
        &self,
        origin_session_ref: &str,
        origin_turn_ref: &str,
        origin_turn_revision: u64,
    ) -> Result<bool, String> {
        self.queue_origin_turn_stop_as(
            origin_session_ref,
            origin_turn_ref,
            origin_turn_revision,
            true,
        )
    }

    fn queue_origin_turn_stop_as(
        &self,
        origin_session_ref: &str,
        origin_turn_ref: &str,
        origin_turn_revision: u64,
        prepared: bool,
    ) -> Result<bool, String> {
        if !super::super::stable_id(origin_session_ref)
            || !super::super::stable_id(origin_turn_ref)
            || origin_turn_revision == 0
        {
            return Err("Invalid conversation turn to stop".into());
        }
        let Some(binding) = self.configured_origin_hub_binding() else {
            return Ok(false);
        };
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if let Some(pending) =
            runtime
                .receipts
                .pending_origin_stop(&binding, origin_session_ref, origin_turn_ref)
        {
            if pending.payload["origin_turn_revision"].as_u64() != Some(origin_turn_revision) {
                return Err(
                    "A different revision of this conversation turn is already stopping".into(),
                );
            }
            return Ok(true);
        }
        runtime
            .receipts
            .insert(Receipt {
                hub: binding,
                user_id: "device".into(),
                operation: if prepared {
                    ReceiptOperation::StopOriginTurnPrepared {
                        origin_session_ref: origin_session_ref.into(),
                        origin_turn_ref: origin_turn_ref.into(),
                    }
                } else {
                    ReceiptOperation::StopOriginTurn {
                        origin_session_ref: origin_session_ref.into(),
                        origin_turn_ref: origin_turn_ref.into(),
                    }
                },
                payload: json!({"request_id":format!("stop-{origin_turn_ref}"),
                "origin_turn_revision":origin_turn_revision}),
            })
            .map_err(|error| error.message().to_string())?;
        Ok(true)
    }

    fn reconcile_prepared_origin_stops(&self, hub_binding: &str) -> Result<(), String> {
        let prepared = self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending_origin_stops(hub_binding);
        for receipt in prepared {
            let ReceiptOperation::StopOriginTurnPrepared {
                origin_session_ref,
                origin_turn_ref,
            } = &receipt.operation
            else {
                continue;
            };
            let session_id = origin_session_ref
                .parse::<crate::session::SessionId>()
                .map_err(|_| "Invalid prepared Stop session".to_string())?;
            let turn_id = origin_turn_ref
                .parse::<crate::protocol::TurnId>()
                .map_err(|_| "Invalid prepared Stop turn".to_string())?;
            let revision = receipt.payload["origin_turn_revision"]
                .as_u64()
                .ok_or_else(|| "Invalid prepared Stop revision".to_string())?;
            if self
                .inner
                .store
                .session_repo()
                .origin_turn_user_stop_committed(session_id, turn_id, revision)
                .map_err(|error| error.to_string())?
            {
                self.inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .receipts
                    .promote_prepared_origin_stop(&receipt)
                    .map_err(|error| error.message().to_string())?;
            } else if self
                .inner
                .store
                .session_repo()
                .origin_turn_terminal_without_user_stop(session_id, turn_id)
                .map_err(|error| error.to_string())?
            {
                // A terminal with another cause can no longer acquire the
                // exact UserStop CAS. This unused reservation is safe to free.
                self.inner
                    .shared_work
                    .0
                    .lock()
                    .unwrap()
                    .receipts
                    .remove_confirmed(&receipt)
                    .map_err(|error| error.message().to_string())?;
            }
        }
        Ok(())
    }

    pub(crate) fn queue_origin_conversation_stop(
        &self,
        origin_session_ref: &str,
        through_revision: u64,
    ) -> Result<bool, String> {
        if !super::super::stable_id(origin_session_ref) || through_revision == 0 {
            return Err("Invalid conversation to stop".into());
        }
        let Some(binding) = self.configured_origin_hub_binding() else {
            return Ok(false);
        };
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if runtime
            .receipts
            .pending_origin_conversation_stop(&binding, origin_session_ref, through_revision)
            .is_some()
        {
            return Ok(true);
        }
        runtime
            .receipts
            .insert(Receipt {
                hub: binding,
                user_id: "device".into(),
                operation: ReceiptOperation::StopOriginConversation {
                    origin_session_ref: origin_session_ref.into(),
                    through_revision,
                },
                payload: json!({"request_id":ulid::Ulid::new().to_string(),
                "through_revision":through_revision}),
            })
            .map_err(|error| error.message().to_string())?;
        Ok(true)
    }

    /// Retry saved exact Stops with the current approved device identity. A lost
    /// reply uses the same request ID and never becomes a new Stop decision.
    pub(crate) fn has_pending_origin_turn_stops(&self) -> bool {
        let Some(connection) = self.shared_connection() else {
            return false;
        };
        !self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending_origin_stops(&connection.hub_binding)
            .is_empty()
    }

    pub(crate) async fn agent_retry_origin_turn_stops(&self) -> Result<(), String> {
        let (connection, session) = self.agent_session().await?;
        self.retry_origin_turn_stops_with_session(&connection, &session)
            .await
    }

    /// Reconnection already established the device transport. This path avoids
    /// calling `resume` from the heartbeat task that is performing the retry.
    pub(crate) async fn retry_origin_turn_stops_connected(&self) -> Result<(), String> {
        let (connection, session) = self.agent_connected_session().await?;
        self.retry_origin_turn_stops_with_session(&connection, &session)
            .await
    }

    pub(super) async fn retry_origin_turn_stops_with_session(
        &self,
        connection: &Connection,
        session: &LoginSession,
    ) -> Result<(), String> {
        let _retry = self.inner.shared_work.1.lock().await;
        self.reconcile_prepared_origin_stops(&connection.hub_binding)?;
        let pending = self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending_origin_stops(&connection.hub_binding);
        let mut first_error = None;
        for receipt in pending {
            let (origin_session_ref, expected_turn) = match &receipt.operation {
                ReceiptOperation::StopOriginTurn {
                    origin_session_ref,
                    origin_turn_ref,
                } => (origin_session_ref, Some(origin_turn_ref.as_str())),
                ReceiptOperation::StopOriginTurnPrepared { .. } => continue,
                ReceiptOperation::StopOriginConversation {
                    origin_session_ref, ..
                } => (origin_session_ref, None),
                _ => continue,
            };
            let acknowledged: AgentOriginTurnStop = match request(
                &connection.client,
                &receipt.operation.path(),
                Some(&session.token),
                Some(receipt.payload.clone()),
                &[],
            )
            .await
            {
                Ok(acknowledged) => acknowledged,
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        format!(
                            "The remote Stop is saved but Hub has not confirmed it: {}",
                            error.message()
                        )
                    });
                    continue;
                }
            };
            if !acknowledged.accepted
                || acknowledged.origin_session_ref != *origin_session_ref
                || acknowledged.origin_turn_ref.as_deref() != expected_turn
                || Some(acknowledged.request_id.as_str()) != receipt.payload["request_id"].as_str()
            {
                first_error.get_or_insert_with(|| "Hub returned a Stop receipt for a different conversation turn; the saved Stop remains pending".to_string());
                continue;
            }
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)
                .map_err(|error| error.message().to_string())?;
        }
        first_error.map_or(Ok(()), Err)
    }
    /// A lost Hub reply may have committed a remote job even before a local
    /// ToolOutput exists. Chat deletion must keep this durable receipt visible.
    pub(crate) fn origin_has_pending_submission(
        &self,
        origin_session_ref: &str,
    ) -> Result<bool, String> {
        if !super::super::stable_id(origin_session_ref) {
            return Err("Invalid local conversation identity".into());
        }
        self.inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .has_origin_submission(origin_session_ref)
            .map_err(|error| error.message().to_string())
    }
    pub(crate) fn origin_turn_has_pending_submission(
        &self,
        origin_session_ref: &str,
        origin_turn_ref: &str,
    ) -> Result<bool, String> {
        if !super::super::stable_id(origin_session_ref) || !super::super::stable_id(origin_turn_ref)
        {
            return Err("Invalid local conversation turn identity".into());
        }
        self.inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .has_origin_turn_submission(origin_session_ref, origin_turn_ref)
            .map_err(|error| error.message().to_string())
    }
    /// Stage an immutable input from the currently assigned Runner for one child.
    /// The Hub checks the device, live attempt, and generation; controller access
    /// is not required on an execution-only PC.
    pub(crate) async fn agent_upload_shared_input(
        &self,
        request_id: &str,
        project_id: &str,
        job_id: &str,
        attempt_id: &str,
        generation: u64,
        name: &str,
        bytes: Vec<u8>,
        admit: impl FnOnce() -> Result<crate::runtime::ToolEffectCommitReservation, String>,
    ) -> Result<Value, String> {
        if ![request_id, project_id, job_id, attempt_id]
            .iter()
            .all(|id| super::super::stable_id(id))
            || generation == 0
            || name.is_empty()
            || name.len() > 512
            || name.starts_with('/')
            || name
                .chars()
                .any(|ch| ch == '\\' || ch == ':' || ch.is_control())
            || name
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || bytes.len() > MAX_AGENT_ARTIFACT_FILE as usize
        {
            return Err("Invalid shared input name, size, or attempt identity".into());
        }
        let (connection, _session) = self.agent_session().await?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_length = bytes.len() as u64;
        let effect_commit = admit()?;
        let uploaded: Result<WorkAsset, RequestError> = request(
            &connection.client,
            &format!("runner/attempts/{attempt_id}/assets"),
            None,
            Some(json!({
                "generation":generation,
                "kind":"input",
                "base_sha256":null,
                "upload":{
                    "request_id":request_id,
                    "name":name,
                    "sha256":sha256,
                    "content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)
                }
            })),
            &[],
        )
        .await;
        effect_commit.release();
        let asset = uploaded.map_err(|error| error.message().to_string())?;
        if asset.project_id != project_id
            || asset.job_id.as_deref() != Some(job_id)
            || asset.name != name
            || asset.sha256 != sha256
            || asset.byte_length != byte_length
            || asset.kind != "input"
            || asset.purged_at_ms.is_some()
        {
            return Err("Hub returned a different shared input identity".into());
        }
        Ok(
            json!({"asset_id":asset.id,"project_id":project_id,"name":asset.name,
            "sha256":asset.sha256,"byte_length":asset.byte_length,"version":asset.version}),
        )
    }

    pub(crate) async fn agent_shared_child_artifacts(
        &self,
        project_id: &str,
        parent_job_id: &str,
        attempt_id: &str,
        generation: u64,
        child_job_id: &str,
    ) -> Result<Value, String> {
        if ![project_id, parent_job_id, attempt_id, child_job_id]
            .iter()
            .all(|id| super::super::stable_id(id))
            || generation == 0
        {
            return Err("Invalid shared child identity".into());
        }
        let (connection, _session) = self.agent_session().await?;
        let generation_text = generation.to_string();
        let listing: SharedChildArtifacts = request(
            &connection.client,
            &format!("runner/attempts/{attempt_id}/children/{child_job_id}/artifacts"),
            None,
            None,
            &[("generation", generation_text.as_str())],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if listing.items.iter().any(|asset| {
            !super::super::stable_id(&asset.id)
                || asset.project_id != project_id
                || asset.job_id.as_deref() != Some(child_job_id)
                || asset.kind != "artifact"
                || asset.purged_at_ms.is_some()
        }) {
            return Err("Hub returned artifacts outside the shared child".into());
        }
        let listing_count = listing.items.len();
        let items = listing
            .items
            .into_iter()
            .take(32)
            .map(|asset| {
                json!({"asset_id":asset.id,"name":asset.name,"sha256":asset.sha256,
                    "byte_length":asset.byte_length,"version":asset.version,
                    "inline_size_ok":asset.byte_length <= MAX_AGENT_ARTIFACT_TEXT,
                    "saveable_here":asset.byte_length <= MAX_AGENT_ARTIFACT_FILE})
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"project_id":project_id,"parent_job_id":parent_job_id,
            "child_job_id":child_job_id,"artifacts":{"items":items,
                "total":listing_count,"more":listing_count > 32}}),
        )
    }

    pub(crate) async fn agent_download_shared_child_artifact(
        &self,
        project_id: &str,
        parent_job_id: &str,
        attempt_id: &str,
        generation: u64,
        child_job_id: &str,
        asset_id: &str,
        max_bytes: u64,
    ) -> Result<(WorkAsset, Vec<u8>), String> {
        if ![
            project_id,
            parent_job_id,
            attempt_id,
            child_job_id,
            asset_id,
        ]
        .iter()
        .all(|id| super::super::stable_id(id))
            || generation == 0
            || max_bytes == 0
            || max_bytes > MAX_AGENT_ARTIFACT_FILE
        {
            return Err("Invalid shared child artifact request".into());
        }
        let (connection, _session) = self.agent_session().await?;
        let generation_text = generation.to_string();
        let download: AgentDownload = request(
            &connection.client,
            &format!("runner/attempts/{attempt_id}/children/{child_job_id}/artifacts/{asset_id}"),
            None,
            None,
            &[("generation", generation_text.as_str())],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let asset = download.asset;
        if asset.id != asset_id
            || asset.project_id != project_id
            || asset.job_id.as_deref() != Some(child_job_id)
            || asset.kind != "artifact"
            || asset.purged_at_ms.is_some()
            || asset.byte_length > max_bytes
            || download.content_base64.len() as u64 > max_bytes.div_ceil(3) * 4
        {
            return Err("Hub returned a different shared child artifact".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(download.content_base64)
            .map_err(|_| "Hub returned invalid shared child artifact encoding".to_string())?;
        if bytes.len() as u64 != asset.byte_length
            || format!("{:x}", Sha256::digest(&bytes)) != asset.sha256
        {
            return Err("Hub returned a different shared child artifact content".into());
        }
        Ok((asset, bytes))
    }

    pub(crate) async fn agent_upload_file(
        &self,
        request_id: &str,
        project_id: &str,
        name: &str,
        bytes: Vec<u8>,
        admit: impl FnOnce() -> Result<crate::runtime::ToolEffectCommitReservation, String>,
    ) -> Result<Value, String> {
        if ![request_id, project_id]
            .iter()
            .all(|id| super::super::stable_id(id))
            || name.is_empty()
            || name.len() > 255
            || name.chars().any(|ch| ch == '/' || ch == '\\')
            || bytes.len() > 8 * 1024 * 1024
        {
            return Err("Invalid file name, size, or project identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let projects: Vec<WorkProject> = request(
            &connection.client,
            "projects",
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if !projects
            .iter()
            .any(|project| project.id == project_id && project.allows_submission())
        {
            return Err("This device cannot upload to the selected project".into());
        }
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_length = bytes.len() as u64;
        let effect_commit = admit()?;
        let uploaded: Result<WorkAsset, RequestError> = request(
            &connection.client,
            &format!("projects/{project_id}/assets"),
            Some(&session.token),
            Some(json!({"request_id":request_id,"name":name,"sha256":sha256,
                "content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})),
            &[],
        )
        .await;
        effect_commit.release();
        let asset = uploaded.map_err(|error| error.message().to_string())?;
        if asset.project_id != project_id
            || asset.name != name
            || asset.sha256 != sha256
            || asset.byte_length != byte_length
            || asset.kind != "input"
            || asset.purged_at_ms.is_some()
        {
            return Err("Hub returned a different file identity".into());
        }
        Ok(
            json!({"asset_id":asset.id,"project_id":project_id,"name":asset.name,
            "sha256":asset.sha256,"byte_length":asset.byte_length,"version":asset.version}),
        )
    }

    pub(super) async fn agent_session(&self) -> Result<(Connection, LoginSession), String> {
        // Desktop resumes its device connection at startup. CLI and TUI create the
        // same service lazily, so the first team tool must resume saved mTLS state.
        if self.shared_connection().is_none() {
            self.resume()
                .await
                .map_err(|error| format!("Hub connection could not be resumed: {error}"))?;
        }
        self.agent_connected_session().await
    }

    async fn agent_connected_session(&self) -> Result<(Connection, LoginSession), String> {
        let connection = self
            .shared_connection()
            .ok_or_else(|| "Hub is not connected from this device".to_string())?;
        let session: LoginSession = request(
            &connection.client,
            "device-session",
            None,
            Some(json!({})),
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if session.token.is_empty()
            || session.token.len() > 4096
            || session.expires_at_ms <= now_ms()
            || !super::super::stable_id(&session.principal.user_id)
            || self
                .shared_connection()
                .is_none_or(|current| current.binding != connection.binding)
        {
            return Err("Hub device identity or session changed".into());
        }
        Ok((connection, session))
    }

    /// Every row is returned by the Hub under this device's current project grant.
    /// Capabilities are declared hints, not proof that a program can be executed.
    pub(crate) async fn agent_environments(
        &self,
        project_id: Option<&str>,
        name: Option<&str>,
    ) -> Result<Value, String> {
        if project_id.is_some_and(|id| !super::super::stable_id(id))
            || name.is_some_and(|name| name.len() > 128 || name.chars().any(char::is_control))
        {
            return Err("Invalid project or PC name".into());
        }
        let (connection, session) = self.agent_session().await?;
        let projects: Vec<WorkProject> = request(
            &connection.client,
            "projects",
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let mut visible = Vec::new();
        for project in projects.iter().filter(|project| {
            project.allows_submission()
                && project_id.is_none_or(|requested| requested == project.id)
        }) {
            let environments: Vec<AgentEnvironment> = request(
                &connection.client,
                "environments",
                Some(&session.token),
                None,
                &[("project_id", &project.id)],
            )
            .await
            .map_err(|error| error.message().to_string())?;
            for environment in environments {
                if name.is_some_and(|requested| {
                    ![
                        &environment.label,
                        environment
                            .device_label
                            .as_ref()
                            .unwrap_or(&environment.label),
                    ]
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(requested))
                }) {
                    continue;
                }
                visible.push(json!({
                    "project_id": project.id,
                    "project_label": project.label,
                    "environment_id": environment.id,
                    "environment_label": environment.label,
                    "device_id": environment.device_id,
                    "device_label": environment.device_label,
                    "enabled": environment.enabled,
                    "can_submit": environment.can_submit,
                    "capacity": environment.capacity,
                    "occupied": environment.occupied,
                    "runner_contact": environment.runner_contact,
                    "capabilities": environment.capabilities,
                }));
            }
        }
        if project_id.is_some()
            && !projects.iter().any(|project| {
                project.allows_submission() && project_id == Some(project.id.as_str())
            })
        {
            return Err("This device cannot submit work to the selected project".into());
        }
        visible.sort_by(|a, b| {
            (a["project_id"].as_str(), a["environment_id"].as_str())
                .cmp(&(b["project_id"].as_str(), b["environment_id"].as_str()))
        });
        let count = visible.len();
        visible.truncate(32);
        Ok(
            json!({"environments":visible,"total_matches":count,"more":count>32,
            "capability_notice":"Declared capabilities require confirmation on the selected PC before use."}),
        )
    }

    /// A durable receipt fences an unknown POST; a new model call cannot silently resubmit it.
    pub(crate) async fn agent_submit_job(
        &self,
        request_id: &str,
        origin_session_ref: &str,
        origin_turn_ref: &str,
        origin_turn_revision: u64,
        project_id: &str,
        environment_id: &str,
        title: &str,
        prompt: &str,
        input_refs: &[String],
        admit: impl FnOnce() -> Result<crate::runtime::ToolEffectCommitReservation, String>,
    ) -> Result<Value, String> {
        if ![
            request_id,
            origin_session_ref,
            origin_turn_ref,
            project_id,
            environment_id,
        ]
        .iter()
        .all(|id| super::super::stable_id(id))
            || origin_turn_revision == 0
            || title.trim().is_empty()
            || title.len() > 256
            || prompt.trim().is_empty()
            || prompt.len() > 32768
            || input_refs.len() > 32
            || input_refs.iter().any(|id| !super::super::stable_id(id))
            || input_refs
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != input_refs.len()
        {
            return Err("Invalid shared work target, title, or prompt".into());
        }
        let (connection, session) = self.agent_session().await?;
        let projects: Vec<WorkProject> = request(
            &connection.client,
            "projects",
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if !projects
            .iter()
            .any(|project| project.id == project_id && project.allows_submission())
        {
            return Err("This device cannot submit work to the selected project".into());
        }
        let environments: Vec<AgentEnvironment> = request(
            &connection.client,
            "environments",
            Some(&session.token),
            None,
            &[("project_id", project_id)],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let selected = environments.iter().find(|environment| {
            environment.id == environment_id && environment.enabled && environment.can_submit
        });
        let Some(selected) = selected else {
            return Err("The selected PC is not available for this project".into());
        };
        let local_device = self.inner.state.lock().unwrap().settings.device_id.clone();
        if selected
            .device_id
            .as_ref()
            .is_some_and(|device| Some(device) == local_device.as_ref())
        {
            return Err("This PC is already running the conversation. Use local tools for work on this PC; delegate only the steps that need another PC.".into());
        }
        let effect_commit = admit()?;
        let admitted: AgentOriginTurnAdmission = match request(
            &connection.client,
            &format!("origins/{origin_session_ref}/turns/{origin_turn_ref}/admit"),
            Some(&session.token),
            Some(json!({"request_id":format!("admit-{origin_turn_ref}"),
                "create":true,"origin_turn_revision":origin_turn_revision})),
            &[],
        )
        .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                effect_commit.release();
                return Err(format!(
                    "Hub could not admit this exact conversation turn: {}",
                    error.message()
                ));
            }
        };
        if admitted.origin_session_ref != origin_session_ref
            || admitted.origin_turn_ref != origin_turn_ref
            || admitted.epoch == 0
        {
            effect_commit.release();
            return Err("Hub returned a different conversation turn admission".into());
        }
        let receipt = Receipt {
            hub: connection.hub_binding.clone(),
            user_id: session.principal.user_id,
            operation: ReceiptOperation::Submit,
            payload: json!({"request_id":request_id,"origin_session_ref":origin_session_ref,
                "origin_turn_ref":origin_turn_ref,"origin_turn_epoch":admitted.epoch,
                "project_id":project_id,
                "environment_id":environment_id,"title":title,
                "input":{"version":2,"prompt":prompt,"input_refs":input_refs},
                "descendant_budget":8,"start_before_ms":now_ms().saturating_add(24*60*60*1000)}),
        };
        self.shared_work_projection();
        {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            runtime
                .receipts
                .insert(receipt.clone())
                .map_err(|error| error.message().to_string())?;
        }
        let result: Result<WorkDetail, RequestError> = request(
            &connection.client,
            "jobs",
            Some(&session.token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        effect_commit.release();
        if let Ok(job) = &result {
            if job.project_id != project_id
                || job.environment_id != environment_id
                || job.origin_session_ref.as_deref() != Some(origin_session_ref)
                || job.origin_turn_ref.as_deref() != Some(origin_turn_ref)
                || job.origin_turn_epoch != Some(admitted.epoch)
                || job.title != title
                || job.input != receipt.payload["input"]
                || !super::super::stable_id(&job.id)
                || !job
                    .conversation_id
                    .as_deref()
                    .is_some_and(super::super::stable_id)
            {
                return Err("Hub returned a job for a different target or chat".into());
            }
        }
        // Hub's idempotent request ID returns the existing job on an accepted
        // retry. A 409 therefore proves this payload was fenced, including by
        // an exact-turn Stop, and must not block a later turn indefinitely.
        if result.is_ok() || matches!(&result, Err(RequestError::Http(400 | 404 | 409))) {
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)
                .map_err(|error| error.message().to_string())?;
        }
        let job = result.map_err(|error| format!(
            "{} If Hub accepted the request but the reply was lost, check the existing submission receipt before sending a new job.",
            error.message()
        ))?;
        Ok(json!({"job_id":job.id,"project_id":job.project_id,
            "environment_id":job.environment_id,"state":job.state,
            "conversation_id":job.conversation_id}))
    }

    pub(crate) async fn agent_job_status(
        &self,
        project_id: &str,
        job_id: &str,
    ) -> Result<Value, String> {
        if !super::super::stable_id(project_id) || !super::super::stable_id(job_id) {
            return Err("Invalid project or job identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let job: WorkDetail = request(
            &connection.client,
            &format!("jobs/{job_id}"),
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if job.project_id != project_id || job.id != job_id {
            return Err("Hub returned a job for a different project".into());
        }
        let mut result = job.result;
        if let Some(value) = result.as_mut() {
            let size = serde_json::to_vec(value)
                .map_err(|_| "Invalid result encoding")?
                .len();
            if size > 64 * 1024 {
                let preview = value["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(16_384)
                    .collect::<String>();
                *value = json!({"truncated":true,"text_preview":preview});
            }
        }
        let artifacts = if matches!(job.state.as_str(), "succeeded" | "failed" | "cancelled") {
            Some(
                self.agent_job_artifacts_with_session(&connection, &session, project_id, job_id)
                    .await?,
            )
        } else {
            None
        };
        Ok(json!({"job_id":job.id,"project_id":job.project_id,
            "conversation_id":job.conversation_id,"state":job.state,
            "environment_id":job.environment_id,"wait_reason":job.wait_reason,
            "uncertainty_reason":job.uncertainty_reason,"result":result,
            "retained_services":job.retained_services,"artifacts":artifacts}))
    }

    /// The normal chat has no Hub-specific Retry button. Replaying its one
    /// uncertain submission must use the durable request body, not a new tool
    /// call ID or a model reconstruction of the original prompt and inputs.
    pub(crate) async fn agent_retry_submission(
        &self,
        origin_session_ref: &str,
        admit: impl FnOnce() -> Result<crate::runtime::ToolEffectCommitReservation, String>,
    ) -> Result<Value, String> {
        if !super::super::stable_id(origin_session_ref) {
            return Err("Invalid local conversation identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let receipt = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            if let Some(error) = runtime.receipts.error() {
                return Err(error.into());
            }
            let pending = runtime
                .receipts
                .pending(&connection.hub_binding, &session.principal.user_id)
                .ok_or_else(|| "This chat has no uncertain team submission to retry".to_string())?;
            if pending.operation != ReceiptOperation::Submit
                || pending.payload["origin_session_ref"].as_str() != Some(origin_session_ref)
            {
                return Err("This chat has no uncertain team submission to retry".into());
            }
            pending.clone()
        };
        let effect_commit = admit()?;
        if self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending(&receipt.hub, &receipt.user_id)
            != Some(&receipt)
        {
            return Err("The pending Hub submission changed; refresh before retrying".into());
        }
        let result: Result<WorkDetail, RequestError> = request(
            &connection.client,
            "jobs",
            Some(&session.token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        effect_commit.release();
        if let Ok(job) = &result {
            if receipt.payload["project_id"].as_str() != Some(job.project_id.as_str())
                || receipt.payload["environment_id"].as_str() != Some(job.environment_id.as_str())
                || job.origin_session_ref.as_deref() != Some(origin_session_ref)
                || job.origin_turn_ref.as_deref() != receipt.payload["origin_turn_ref"].as_str()
                || job.origin_turn_epoch != receipt.payload["origin_turn_epoch"].as_u64()
                || receipt.payload["title"].as_str() != Some(job.title.as_str())
                || job.input != receipt.payload["input"]
                || !super::super::stable_id(&job.id)
                || !job
                    .conversation_id
                    .as_deref()
                    .is_some_and(super::super::stable_id)
            {
                return Err("Hub returned a job for a different saved submission".into());
            }
        }
        if result.is_ok() || matches!(&result, Err(RequestError::Http(400 | 404 | 409))) {
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)
                .map_err(|error| error.message().to_string())?;
        }
        let job = result.map_err(|error| {
            format!(
                "{} The same Hub request ID remains saved if the reply is still uncertain.",
                error.message()
            )
        })?;
        Ok(json!({"job_id":job.id,"project_id":job.project_id,
            "environment_id":job.environment_id,"state":job.state,
            "conversation_id":job.conversation_id,"recovered":true}))
    }

    async fn agent_job_artifacts_with_session(
        &self,
        connection: &Connection,
        session: &LoginSession,
        project_id: &str,
        job_id: &str,
    ) -> Result<Value, String> {
        let assets: Vec<WorkAsset> = request(
            &connection.client,
            &format!("jobs/{job_id}/assets"),
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let mut artifacts = Vec::new();
        for asset in assets {
            if asset.project_id != project_id {
                return Err("Hub returned an artifact from another project".into());
            }
            if asset.job_id.as_deref() == Some(job_id)
                && asset.kind == "artifact"
                && asset.purged_at_ms.is_none()
            {
                artifacts.push(
                    json!({"asset_id":asset.id,"name":asset.name,"sha256":asset.sha256,
                    "byte_length":asset.byte_length,"version":asset.version,
                    "inline_size_ok":asset.byte_length <= MAX_AGENT_ARTIFACT_TEXT,
                    "saveable_here":asset.byte_length <= MAX_AGENT_ARTIFACT_FILE}),
                );
            }
        }
        let total = artifacts.len();
        artifacts.truncate(32);
        Ok(json!({"items":artifacts,"total":total,"more":total>32}))
    }

    pub(crate) async fn agent_read_artifact_text(
        &self,
        project_id: &str,
        job_id: &str,
        asset_id: &str,
    ) -> Result<Value, String> {
        let (asset, bytes) = self
            .agent_download_artifact_bounded(project_id, job_id, asset_id, MAX_AGENT_ARTIFACT_TEXT)
            .await?;
        let content = String::from_utf8(bytes).map_err(|_| {
            "This artifact is not UTF-8 text; use team_save_artifact to save it in the workspace"
                .to_string()
        })?;
        Ok(
            json!({"asset_id":asset.id,"project_id":project_id,"job_id":job_id,
            "name":asset.name,"sha256":asset.sha256,"byte_length":asset.byte_length,
            "content":content}),
        )
    }

    pub(crate) async fn agent_download_artifact(
        &self,
        project_id: &str,
        job_id: &str,
        asset_id: &str,
    ) -> Result<(WorkAsset, Vec<u8>), String> {
        self.agent_download_artifact_bounded(project_id, job_id, asset_id, MAX_AGENT_ARTIFACT_FILE)
            .await
    }

    async fn agent_download_artifact_bounded(
        &self,
        project_id: &str,
        job_id: &str,
        asset_id: &str,
        max_bytes: u64,
    ) -> Result<(WorkAsset, Vec<u8>), String> {
        if ![project_id, job_id, asset_id]
            .iter()
            .all(|id| super::super::stable_id(id))
        {
            return Err("Invalid project, job, or artifact identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let job: WorkDetail = request(
            &connection.client,
            &format!("jobs/{job_id}"),
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        if job.id != job_id || job.project_id != project_id {
            return Err("The artifact job belongs to another project".into());
        }
        let assets: Vec<WorkAsset> = request(
            &connection.client,
            &format!("jobs/{job_id}/assets"),
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let asset = assets
            .into_iter()
            .find(|asset| {
                asset.id == asset_id
                    && asset.project_id == project_id
                    && asset.job_id.as_deref() == Some(job_id)
                    && asset.kind == "artifact"
                    && asset.purged_at_ms.is_none()
            })
            .ok_or_else(|| "This artifact is not available for the selected job".to_string())?;
        if asset.byte_length > max_bytes {
            return Err(format!(
                "This artifact exceeds the {max_bytes}-byte limit for this action"
            ));
        }
        let download: AgentDownload = request(
            &connection.client,
            &format!("assets/{asset_id}"),
            Some(&session.token),
            None,
            &[],
        )
        .await
        .map_err(|error| error.message().to_string())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(download.content_base64)
            .map_err(|_| "Hub returned invalid artifact encoding".to_string())?;
        if download.asset.id != asset.id
            || download.asset.project_id != project_id
            || download.asset.job_id.as_deref() != Some(job_id)
            || download.asset.kind != "artifact"
            || download.asset.name != asset.name
            || download.asset.version != asset.version
            || download.asset.byte_length != asset.byte_length
            || download.asset.purged_at_ms.is_some()
            || download.asset.sha256 != asset.sha256
            || bytes.len() as u64 > max_bytes
            || bytes.len() as u64 != asset.byte_length
            || format!("{:x}", Sha256::digest(&bytes)) != asset.sha256
        {
            return Err("Hub returned a different artifact identity or content".into());
        }
        Ok((asset, bytes))
    }

    pub(crate) async fn agent_stop_service(
        &self,
        service_id: &str,
        admit: impl FnOnce() -> Result<crate::runtime::ToolEffectCommitReservation, String>,
    ) -> Result<Value, String> {
        if !super::super::stable_id(service_id) {
            return Err("Invalid service identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let effect_commit = admit()?;
        let result: Result<Value, RequestError> = request(
            &connection.client,
            &format!("services/{service_id}/stop"),
            Some(&session.token),
            Some(json!({})),
            &[],
        )
        .await;
        effect_commit.release();
        let state = result.map_err(|error| error.message().to_string())?;
        if state["service_id"].as_str() != Some(service_id) {
            return Err("Hub returned a different service identity".into());
        }
        Ok(state)
    }

    /// Stop every service that belonged to this conversation when Hub accepted
    /// the exact request. A lost reply reuses the durable request ID, so a retry
    /// cannot silently stop a service started later in the same conversation.
    pub(crate) async fn agent_stop_conversation<G>(
        &self,
        request_id: &str,
        project_id: &str,
        conversation_id: &str,
        admit: impl FnOnce() -> Result<G, String>,
    ) -> Result<Value, String> {
        if ![request_id, project_id, conversation_id]
            .iter()
            .all(|id| super::super::stable_id(id))
        {
            return Err("Invalid project, conversation, or stop request identity".into());
        }
        let (connection, session) = self.agent_session().await?;
        let requested = Receipt {
            hub: connection.hub_binding.clone(),
            user_id: session.principal.user_id.clone(),
            operation: ReceiptOperation::StopConversation {
                conversation_id: conversation_id.to_owned(),
            },
            payload: json!({"project_id":project_id,"request_id":request_id}),
        };
        let retry = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            if let Some(error) = runtime.receipts.error() {
                return Err(error.into());
            }
            match runtime.receipts.pending(&requested.hub, &requested.user_id) {
                Some(existing)
                    if existing.operation == requested.operation
                        && existing.payload["project_id"] == project_id =>
                {
                    Some(existing.clone())
                }
                Some(_) => {
                    return Err("Confirm the previous uncertain Hub request before stopping this conversation".into());
                }
                None => None,
            }
        };
        let is_retry = retry.is_some();
        let receipt = retry.unwrap_or(requested);
        let effect_commit = admit()?;
        {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if !is_retry {
                runtime
                    .receipts
                    .insert(receipt.clone())
                    .map_err(|error| error.message().to_string())?;
            } else if runtime.receipts.pending(&receipt.hub, &receipt.user_id) != Some(&receipt) {
                return Err("The pending Hub request changed; refresh before retrying".into());
            }
        }
        let result: Result<Value, RequestError> = request(
            &connection.client,
            &receipt.operation.path(),
            Some(&session.token),
            Some(receipt.payload.clone()),
            &[],
        )
        .await;
        drop(effect_commit);
        let definite_rejection = matches!(result, Err(RequestError::Http(400 | 404)));
        if definite_rejection {
            self.inner
                .shared_work
                .0
                .lock()
                .unwrap()
                .receipts
                .remove_confirmed(&receipt)
                .map_err(|error| error.message().to_string())?;
        }
        let acknowledgment = result.map_err(|error| {
            if definite_rejection {
                error.message().to_string()
            } else {
                format!(
                    "{} The stop request ID is saved. Retry this conversation stop after checking Hub; do not create a different stop request.",
                    error.message()
                )
            }
        })?;
        if acknowledgment["project_id"] != project_id
            || acknowledgment["conversation_id"] != conversation_id
            || acknowledgment["request_id"] != receipt.payload["request_id"]
            || acknowledgment["accepted"].as_bool() != Some(true)
        {
            return Err("Hub returned a different conversation stop acknowledgment".into());
        }
        self.inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .remove_confirmed(&receipt)
            .map_err(|error| error.message().to_string())?;
        Ok(acknowledgment)
    }
}
