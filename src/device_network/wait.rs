use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use super::outgoing::{public_row, terminal};
use super::{DeviceDelegationRow, DeviceNetworkService};
use crate::error::ToolError;
use crate::runtime::RunControl;
use crate::session::SessionId;

#[derive(Debug, Serialize)]
pub(crate) struct DeviceWaitResult {
    pub timed_out: bool,
    pub jobs: Vec<DeviceDelegationRow>,
    pub cursor: BTreeMap<String, String>,
}

impl DeviceNetworkService {
    /// Wait for one of this session's delegated jobs without performing model or
    /// network requests. Existing authenticated synchronization owns observations.
    /// Dropping this future stops waiting, not the remote jobs themselves.
    pub(crate) async fn wait_remote_tasks(
        &self,
        session: SessionId,
        job_ids: &[String],
        after: Option<&BTreeMap<String, String>>,
        timeout: Duration,
        control: RunControl,
    ) -> Result<DeviceWaitResult, ToolError> {
        if job_ids.is_empty()
            || job_ids.len() > 8
            || job_ids.iter().any(|job| job.parse::<ulid::Ulid>().is_err())
            || job_ids.iter().collect::<HashSet<_>>().len() != job_ids.len()
            || timeout > Duration::from_secs(3600)
            || after.is_some_and(|after| {
                after.len() > 8
                    || after.iter().any(|(job, digest)| {
                        !job_ids.contains(job)
                            || digest.len() != 64
                            || !digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
            })
        {
            return Err(ToolError::Message(
                "invalid remote wait targets or deadline".into(),
            ));
        }
        // Subscribe before reading to cover an observation committed at entry.
        // Poll the durable owner as well: another process/store can update it.
        let mut updates = self.inner.outgoing.updates.subscribe();
        let deadline = Instant::now() + timeout;
        let cancel = control.token();
        let identity = {
            let state = self.inner.state.lock().unwrap();
            (
                state.settings.hub_id.clone(),
                state.settings.device_id.clone(),
                state.client.is_some(),
            )
        };
        loop {
            if control.is_cancelled() {
                return Err(ToolError::RunInterrupted);
            }
            {
                let state = self.inner.state.lock().unwrap();
                if state.closing
                    || identity
                        != (
                            state.settings.hub_id.clone(),
                            state.settings.device_id.clone(),
                            state.client.is_some(),
                        )
                {
                    return Err(ToolError::Message(
                        "remote wait connection owner changed".into(),
                    ));
                }
            }
            let jobs = self.remote_wait_snapshot(session, job_ids)?;
            let cursor = jobs
                .iter()
                .map(|job| {
                    // Only semantic observations enter the cursor. Poll timestamps
                    // and repeated delivery cannot wake the model again.
                    let bytes = serde_json::to_vec(&(
                        &job.reference_id,
                        &job.state,
                        &job.stop_status,
                        &job.result,
                    ))
                    .map_err(|_| ToolError::Message("invalid remote wait observation".into()))?;
                    Ok((
                        job.job_id.clone().expect("exact job lookup"),
                        format!("{:x}", Sha256::digest(bytes)),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, ToolError>>()?;
            let ready = jobs.iter().any(|job| {
                let id = job.job_id.as_ref().expect("exact job lookup");
                (terminal(&job.state) || job.state == "awaiting_approval")
                    && after.and_then(|after| after.get(id)) != cursor.get(id)
            });
            if ready || Instant::now() >= deadline {
                if control.is_cancelled() {
                    return Err(ToolError::RunInterrupted);
                }
                return Ok(DeviceWaitResult {
                    timed_out: !ready,
                    jobs,
                    cursor,
                });
            }
            // No outgoing command lane, identity-rotation lease, or model permit
            // is held here. Unchanged progress wakes Rust only, never the model.
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ToolError::RunInterrupted),
                _ = updates.changed() => {},
                _ = tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_secs(1))) => {},
            }
        }
    }

    fn remote_wait_snapshot(
        &self,
        session: SessionId,
        job_ids: &[String],
    ) -> Result<Vec<DeviceDelegationRow>, ToolError> {
        let store = self.inner.store.remote_job_store();
        job_ids
            .iter()
            .map(|job| {
                let row = store
                    .find_device_reference(session, None, Some(job))
                    .map_err(|_| ToolError::Message("remote wait observation unavailable".into()))?
                    .filter(|row| row.session_id == session)
                    .ok_or_else(|| {
                        ToolError::Message(
                            "remote wait target does not belong to this session".into(),
                        )
                    })?;
                Ok(public_row(row))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use crate::device_network::DirectoryPeer;
    use crate::protocol::TurnId;
    use crate::remote_agent::store::StoredDeviceReference;
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
    use camino::Utf8PathBuf;
    use ulid::Ulid;

    async fn fixture() -> (
        tempfile::TempDir,
        DeviceNetworkService,
        StoredDeviceReference,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
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
        let peer = DirectoryPeer {
            device_id: "worker".into(),
            profile_id: "profile".into(),
            label: "Worker".into(),
            name: "temp".into(),
            endpoint: "https://127.0.0.1:7332/mcp".into(),
            mode: "agent".into(),
            scope_id: "scope".into(),
            certificate_pem: "test".into(),
            certificate_sha256: "a".repeat(64),
        };
        let row = StoredDeviceReference {
            id: Ulid::new(),
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            device_id: peer.device_id.clone(),
            profile_id: peer.profile_id.clone(),
            root_task_id: "root".into(),
            request_key: "request".into(),
            prompt_hash: "a".repeat(64),
            parent_grant_id: None,
            parent_job_id: None,
            peer,
            claims: None,
            job_id: Some(Ulid::new().to_string()),
            state: "running".into(),
            stop_status: "none".into(),
            result: None,
        };
        let row = network
            .inner
            .store
            .remote_job_store()
            .accept_device_reference(&row)
            .unwrap();
        (temp, network, row)
    }

    fn save(network: &DeviceNetworkService, row: &StoredDeviceReference) {
        network
            .inner
            .store
            .remote_job_store()
            .update_device_reference(row)
            .unwrap();
        network.inner.outgoing.updates.send_replace(());
    }

    #[tokio::test]
    async fn remote_wait_ignores_unchanged_observations_and_wakes_for_any_exact_target() {
        let (_temp, network, first) = fixture().await;
        let mut second = first.clone();
        second.id = Ulid::new();
        second.job_id = Some(Ulid::new().to_string());
        second.request_key = "second".into();
        let mut second = network
            .inner
            .store
            .remote_job_store()
            .accept_device_reference(&second)
            .unwrap();
        let ids = vec![
            first.job_id.clone().unwrap(),
            second.job_id.clone().unwrap(),
        ];
        let wait = network.wait_remote_tasks(
            first.session_id,
            &ids,
            None,
            Duration::from_secs(3),
            RunControl::new(),
        );
        tokio::pin!(wait);
        // A real pending future proves that receiving poll updates does not finish
        // the tool, and therefore cannot trigger another model request.
        for _ in 0..3 {
            save(&network, &first);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut wait)
                    .await
                    .is_err()
            );
            assert!(network.inner.outgoing.lane.try_lock().is_ok());
        }
        second.state = "completed".into();
        second.result = Some("worker result".into());
        save(&network, &second);
        let result = tokio::time::timeout(Duration::from_millis(100), &mut wait)
            .await
            .unwrap()
            .unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.jobs[0].state, "running");
        assert_eq!(result.jobs[1].result.as_deref(), Some("worker result"));
    }

    #[tokio::test]
    async fn remote_wait_checks_entry_completion_timeout_and_other_owners() {
        let (_temp, network, mut row) = fixture().await;
        let ids = vec![row.job_id.clone().unwrap()];
        assert!(
            network
                .wait_remote_tasks(
                    SessionId::new(),
                    &ids,
                    None,
                    Duration::ZERO,
                    RunControl::new()
                )
                .await
                .is_err()
        );
        assert!(
            network
                .wait_remote_tasks(
                    row.session_id,
                    &[Ulid::new().to_string()],
                    None,
                    Duration::ZERO,
                    RunControl::new()
                )
                .await
                .is_err()
        );
        let timeout = network
            .wait_remote_tasks(
                row.session_id,
                &ids,
                None,
                Duration::from_millis(5),
                RunControl::new(),
            )
            .await
            .unwrap();
        assert!(timeout.timed_out);
        assert_eq!(timeout.jobs[0].state, "running");
        row.state = "completed".into();
        row.result = Some("saved before wait".into());
        save(&network, &row);
        let ready = network
            .wait_remote_tasks(
                row.session_id,
                &ids,
                None,
                Duration::ZERO,
                RunControl::new(),
            )
            .await
            .unwrap();
        assert!(!ready.timed_out);
        assert_eq!(ready.jobs[0].result.as_deref(), Some("saved before wait"));
    }

    #[tokio::test]
    async fn remote_wait_cursor_suppresses_repeated_approval_and_completed_targets() {
        let (_temp, network, mut row) = fixture().await;
        row.state = "awaiting_approval".into();
        save(&network, &row);
        let ids = vec![row.job_id.clone().unwrap()];
        let first = network
            .wait_remote_tasks(
                row.session_id,
                &ids,
                None,
                Duration::ZERO,
                RunControl::new(),
            )
            .await
            .unwrap();
        assert!(!first.timed_out);
        let repeated = network
            .wait_remote_tasks(
                row.session_id,
                &ids,
                Some(&first.cursor),
                Duration::from_millis(10),
                RunControl::new(),
            )
            .await
            .unwrap();
        assert!(repeated.timed_out);
        assert_eq!(first.cursor, repeated.cursor);
        // Completion before the next invocation is still observed; no entry-time
        // baseline can accidentally swallow that transition.
        row.state = "completed".into();
        row.result = Some("approved and finished".into());
        save(&network, &row);
        let complete = network
            .wait_remote_tasks(
                row.session_id,
                &ids,
                Some(&first.cursor),
                Duration::ZERO,
                RunControl::new(),
            )
            .await
            .unwrap();
        assert!(!complete.timed_out);
        let mut second = row.clone();
        second.id = Ulid::new();
        second.turn_id = TurnId::new();
        second.request_key = "another-turn".into();
        second.job_id = Some(Ulid::new().to_string());
        second.state = "running".into();
        second.result = None;
        let mut second = network
            .inner
            .store
            .remote_job_store()
            .accept_device_reference(&second)
            .unwrap();
        let both = vec![ids[0].clone(), second.job_id.clone().unwrap()];
        let wait = network.wait_remote_tasks(
            row.session_id,
            &both,
            Some(&complete.cursor),
            Duration::from_secs(3),
            RunControl::new(),
        );
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        second.state = "completed".into();
        second.result = Some("second finished".into());
        save(&network, &second);
        assert!(!wait.await.unwrap().timed_out);
    }

    #[tokio::test]
    async fn remote_wait_detects_durable_update_without_signal_and_owner_shutdown() {
        let (_temp, network, mut row) = fixture().await;
        let ids = vec![row.job_id.clone().unwrap()];
        let wait = network.wait_remote_tasks(
            row.session_id,
            &ids,
            None,
            Duration::from_secs(3),
            RunControl::new(),
        );
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        row.state = "failed".into();
        network
            .inner
            .store
            .remote_job_store()
            .update_device_reference(&row)
            .unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_millis(1500), &mut wait)
                .await
                .unwrap()
                .unwrap()
                .timed_out
        );
        network.begin_shutdown();
        assert!(
            network
                .wait_remote_tasks(
                    row.session_id,
                    &ids,
                    None,
                    Duration::ZERO,
                    RunControl::new()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn remote_wait_cancel_does_not_change_remote_execution_or_hold_a_lane() {
        let (_temp, network, row) = fixture().await;
        let ids = vec![row.job_id.clone().unwrap()];
        let control = RunControl::new();
        let wait = network.wait_remote_tasks(
            row.session_id,
            &ids,
            None,
            Duration::from_secs(30),
            control.clone(),
        );
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        control.interrupt(crate::protocol::TurnInterruptionCause::UserStop);
        assert!(matches!(wait.await, Err(ToolError::RunInterrupted)));
        assert_eq!(
            network
                .inner
                .store
                .remote_job_store()
                .device_reference(row.id)
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert!(network.inner.outgoing.lane.try_lock().is_ok());
    }
}
