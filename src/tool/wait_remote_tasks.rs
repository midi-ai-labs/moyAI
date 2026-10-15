use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{ResolvedConfig, ToolOutputConfig};
use crate::device_network::DeviceWaitResult;
use crate::error::ToolError;
use crate::runtime::RunControl;
use crate::storage::StoragePaths;
use crate::tool::context::ToolContext;
use crate::tool::multi_agent::wait_for_durable_turn_steer;
use crate::tool::registry::Tool;
use crate::tool::truncate::ToolTruncator;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const DURABLE_STEER_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub struct WaitRemoteTasksTool;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitRemoteTasksInput {
    job_ids: Vec<String>,
    #[serde(default)]
    after: Option<BTreeMap<String, String>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

impl WaitRemoteTasksInput {
    fn validate(&self) -> Result<Duration, ToolError> {
        if self.job_ids.is_empty() || self.job_ids.len() > 8 {
            return Err(ToolError::Message(
                "wait_remote_tasks requires 1 to 8 job IDs".into(),
            ));
        }
        let mut unique = HashSet::new();
        for id in &self.job_ids {
            if id.parse::<ulid::Ulid>().is_err() || !unique.insert(id) {
                return Err(ToolError::Message(
                    "wait_remote_tasks requires distinct valid job IDs returned by delegate_task"
                        .into(),
                ));
            }
        }
        if self.after.as_ref().is_some_and(|cursor| {
            cursor.len() > 8
                || cursor.iter().any(|(job, digest)| {
                    !unique.contains(job)
                        || digest.len() != 64
                        || !digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
        }) {
            return Err(ToolError::Message("wait_remote_tasks after must contain only requested job IDs and their returned cursor values".into()));
        }
        let timeout = self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout) {
            return Err(ToolError::Message(format!(
                "wait_remote_tasks timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
            )));
        }
        Ok(Duration::from_millis(timeout))
    }
}

pub(crate) fn remote_wait_available(config: &ResolvedConfig) -> bool {
    config.device_network.configured()
        || (config.mcp.enabled
            && config.mcp.servers.iter().any(|server| {
                server.enabled && server.remote_agent && server.id.starts_with("hub-device-")
            }))
}

#[async_trait(?Send)]
impl Tool for WaitRemoteTasksTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::WaitRemoteTasks,
            effect: ToolEffectPolicy::read(),
            description: "Wait for saved observations of Hub remote tasks delegated by this session, even while peers are offline. Use job IDs returned by mcp_call delegate_task after doing independent work. Omit timeout_ms for the normal 10-minute wait instead of short polling. On later waits, pass the returned cursor as after, optionally removing finished jobs. New user input ends the wait early; resuming observes existing jobs without resubmitting them.",
            input_schema: json!({
                "type": "object", "required": ["job_ids"], "additionalProperties": false,
                "properties": {
                    "job_ids": { "type": "array", "minItems": 1, "maxItems": 8, "uniqueItems": true,
                        "items": { "type": "string", "minLength": 26, "maxLength": 26 },
                        "description": "Job IDs returned by delegate_task in this session." },
                    "after": { "type": "object", "maxProperties": 8,
                        "additionalProperties": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                        "description": "Cursor returned by the previous wait, restricted to job_ids being waited on." },
                    "timeout_ms": { "type": "integer", "minimum": MIN_TIMEOUT_MS,
                        "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: WaitRemoteTasksInput = serde_json::from_value(raw_arguments)?;
        let timeout = input.validate()?;
        if !remote_wait_available(ctx.config) {
            return Err(ToolError::Message("wait_remote_tasks requires Hub device-network configuration or an enabled Hub remote-agent connection".into()));
        }
        let network = ctx.services.store.device_network().ok_or_else(|| {
            ToolError::Message(
                "The Hub device runtime is unavailable; existing tasks were not resubmitted".into(),
            )
        })?;
        if ctx.run_control.is_cancelled() {
            return Err(ToolError::RunInterrupted);
        }
        let session = ctx.session.session.id;
        let active_runs = ctx.services.store.active_runs();
        let generation = active_runs
            .steer_generation(session)
            .map_err(|error| ToolError::Message(error.to_string()))?;
        let wait = network.wait_remote_tasks(
            session,
            &input.job_ids,
            input.after.as_ref(),
            timeout,
            ctx.run_control.clone(),
        );
        let steer = async {
            active_runs
                .wait_for_steer_activity(session, generation)
                .await
                .map(|_| ())
                .map_err(|error| ToolError::Message(error.to_string()))
        };
        let result = wait_or_steer(
            wait,
            steer,
            || ctx.run_mutation_fence.has_pending_turn_steer_input(),
            &ctx.run_control,
            DURABLE_STEER_POLL,
        )
        .await?;
        wait_result(
            result,
            &input.job_ids,
            input.after.as_ref(),
            &ctx.services.truncator,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )
    }
}

async fn wait_or_steer(
    wait: impl Future<Output = Result<DeviceWaitResult, ToolError>>,
    steer: impl Future<Output = Result<(), ToolError>>,
    mut has_pending: impl FnMut() -> Result<bool, ToolError>,
    control: &RunControl,
    poll: Duration,
) -> Result<Option<DeviceWaitResult>, ToolError> {
    if control.is_cancelled() {
        return Err(ToolError::RunInterrupted);
    }
    // Capture the process-local steer generation before this durable queue check.
    if has_pending()? {
        return Ok(None);
    }
    let cancel = control.token();
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ToolError::RunInterrupted),
        steered = steer => { steered?; None }
        durable = wait_for_durable_turn_steer(|| has_pending().map_err(|error| error.to_string()), poll) => {
            durable.map_err(ToolError::Message)?; None
        }
        result = wait => Some(result?),
    };
    if control.is_cancelled() {
        return Err(ToolError::RunInterrupted);
    }
    // Another store can commit input between the final poll and task/timeout completion.
    if result.is_some() && has_pending()? {
        return Ok(None);
    }
    Ok(result)
}

fn wait_result(
    result: Option<DeviceWaitResult>,
    requested: &[String],
    after: Option<&BTreeMap<String, String>>,
    truncator: &ToolTruncator,
    limits: &ToolOutputConfig,
    paths: &StoragePaths,
) -> Result<ToolResult, ToolError> {
    let interrupted = result.is_none();
    let (timed_out, jobs, cursor) = match result {
        Some(result) => (result.timed_out, result.jobs, result.cursor),
        None => (false, Vec::new(), after.cloned().unwrap_or_default()),
    };
    let metadata_jobs = jobs
        .iter()
        .map(|job| {
            json!({
                "reference_id": job.reference_id, "job_id": job.job_id, "device_id": job.device_id,
                "profile_id": job.profile_id, "state": job.state, "stop_status": job.stop_status,
                "result_received": job.result.is_some(),
            })
        })
        .collect::<Vec<_>>();
    let output = json!({ "interrupted": interrupted, "timed_out": timed_out,
        "requested_job_ids": requested, "jobs": jobs, "cursor": cursor,
        "message": if interrupted { "Wait interrupted by new user input. Existing tasks continue; no replacement task was submitted." }
            else if timed_out { "Wait timed out. These are saved observations, not proof of a remote stop. Resume with the same job IDs and pass cursor as after when needed." }
            else { "Saved remote task observations are available." } });
    let truncated = truncator.preview(serde_json::to_string_pretty(&output)?, limits, paths)?;
    Ok(ToolResult {
        title: if interrupted {
            "遠隔タスクの待機を新しい指示で中断"
        } else if timed_out {
            "遠隔タスクの待機時間が経過"
        } else {
            "遠隔タスクの状態を確認"
        }
        .into(),
        output_text: truncated.preview_text,
        metadata: json!({ "interrupted": interrupted, "timed_out": timed_out,
            "requested_job_ids": requested, "jobs": metadata_jobs, "cursor": cursor,
            "truncated": truncated.truncated }),
        truncated_output_path: truncated.truncated_output_path,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: truncated.internal_file_lease,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::pending;

    use camino::Utf8PathBuf;

    use super::*;
    use crate::device_network::DeviceDelegationRow;
    use crate::protocol::TurnInterruptionCause;

    fn job_ids(count: usize) -> Vec<String> {
        (0..count).map(|_| ulid::Ulid::new().to_string()).collect()
    }

    fn snapshot(timed_out: bool) -> DeviceWaitResult {
        DeviceWaitResult {
            timed_out,
            jobs: Vec::new(),
            cursor: BTreeMap::new(),
        }
    }

    #[test]
    fn remote_wait_schema_and_arguments_bound_targets_and_deadline() {
        let ids = job_ids(8);
        let default: WaitRemoteTasksInput =
            serde_json::from_value(json!({"job_ids": ids})).unwrap();
        assert_eq!(default.validate().unwrap(), Duration::from_secs(600));
        let schema = WaitRemoteTasksTool.spec().input_schema;
        assert_eq!(schema["properties"]["timeout_ms"]["default"], 600_000);
        assert_eq!(schema["properties"]["job_ids"]["maxItems"], 8);
        assert_eq!(WaitRemoteTasksTool.spec().effect, ToolEffectPolicy::read());
        for timeout in [1_000, 3_600_000] {
            let input: WaitRemoteTasksInput =
                serde_json::from_value(json!({"job_ids": [&ids[0]], "timeout_ms": timeout}))
                    .unwrap();
            assert_eq!(input.validate().unwrap(), Duration::from_millis(timeout));
        }
        let cursor = BTreeMap::from([(ids[0].clone(), "a".repeat(64))]);
        let with_cursor: WaitRemoteTasksInput =
            serde_json::from_value(json!({"job_ids": ids, "after": cursor})).unwrap();
        assert!(with_cursor.validate().is_ok());
        for raw in [
            json!({"job_ids": []}),
            json!({"job_ids": job_ids(9)}),
            json!({"job_ids": [&ids[0], &ids[0]]}),
            json!({"job_ids": [""]}),
            json!({"job_ids": ["not-a-delegated-job"]}),
            json!({"job_ids": [" ".repeat(128)]}),
            json!({"job_ids": [&ids[0]], "timeout_ms": 999}),
            json!({"job_ids": [&ids[0]], "timeout_ms": 3_600_001_u64}),
            json!({"job_ids": [&ids[0]], "timeout_ms": -1}),
            json!({"job_ids": [&ids[0]], "timeout_ms": 1.5}),
            json!({"job_ids": [&ids[0]], "unexpected": true}),
            json!({"job_ids": [&ids[1]], "after": cursor}),
            json!({"job_ids": [&ids[0]], "after": {ids[0].clone(): "A".repeat(64)}}),
            json!({"job_ids": [&ids[0]], "after": {ids[0].clone(): "a".repeat(63)}}),
            json!({"job_ids": [&ids[0]], "after": {ids[0].clone(): "z".repeat(64)}}),
            json!({}),
        ] {
            let accepted = serde_json::from_value::<WaitRemoteTasksInput>(raw.clone())
                .ok()
                .is_some_and(|input| input.validate().is_ok());
            assert!(!accepted, "unexpectedly accepted {raw}");
        }
    }

    #[tokio::test]
    async fn remote_wait_prequeued_steer_returns_without_polling_tasks() {
        let polled = Cell::new(false);
        let result = wait_or_steer(
            async {
                polled.set(true);
                Ok(snapshot(false))
            },
            pending(),
            || Ok(true),
            &RunControl::new(),
            DURABLE_STEER_POLL,
        )
        .await
        .unwrap();
        assert!(result.is_none());
        assert!(!polled.get());
    }

    #[tokio::test]
    async fn remote_wait_process_local_steer_ends_pending_wait() {
        let result = wait_or_steer(
            pending(),
            async { Ok(()) },
            || Ok(false),
            &RunControl::new(),
            DURABLE_STEER_POLL,
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn remote_wait_durable_steer_is_observed_without_local_notification() {
        let checks = Cell::new(0);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            wait_or_steer(
                pending(),
                pending(),
                || {
                    checks.set(checks.get() + 1);
                    Ok(checks.get() >= 2)
                },
                &RunControl::new(),
                Duration::from_millis(1),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_none());
        assert_eq!(checks.get(), 2);
    }

    #[tokio::test]
    async fn remote_wait_final_durable_check_wins_over_completion_and_timeout() {
        for timed_out in [false, true] {
            let checks = Cell::new(0);
            let result = wait_or_steer(
                async { Ok(snapshot(timed_out)) },
                pending(),
                || {
                    checks.set(checks.get() + 1);
                    Ok(checks.get() >= 2)
                },
                &RunControl::new(),
                DURABLE_STEER_POLL,
            )
            .await
            .unwrap();
            assert!(result.is_none());
            assert_eq!(checks.get(), 2);
        }
    }

    #[tokio::test]
    async fn remote_wait_own_cancel_is_run_interrupted_at_entry_and_while_waiting() {
        let control = RunControl::new();
        control.interrupt(TurnInterruptionCause::UserStop);
        let entry = wait_or_steer(
            pending(),
            pending(),
            || Ok(true),
            &control,
            DURABLE_STEER_POLL,
        )
        .await;
        assert!(matches!(entry, Err(ToolError::RunInterrupted)));

        let control = RunControl::new();
        let waiting = async {
            control.interrupt(TurnInterruptionCause::UserStop);
            pending::<Result<DeviceWaitResult, ToolError>>().await
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            wait_or_steer(
                waiting,
                pending(),
                || Ok(false),
                &control,
                DURABLE_STEER_POLL,
            ),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ToolError::RunInterrupted)));
    }

    #[tokio::test]
    async fn remote_wait_preserves_observations_and_backend_rejections() {
        let result = wait_or_steer(
            async { Ok(snapshot(true)) },
            pending(),
            || Ok(false),
            &RunControl::new(),
            DURABLE_STEER_POLL,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.timed_out);
        let rejected = wait_or_steer(
            async { Err(ToolError::Message("foreign session".into())) },
            pending(),
            || Ok(false),
            &RunControl::new(),
            DURABLE_STEER_POLL,
        )
        .await;
        assert!(
            matches!(rejected, Err(ToolError::Message(message)) if message == "foreign session")
        );
    }

    fn output_paths(temp: &tempfile::TempDir) -> StoragePaths {
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
        StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        }
    }

    #[test]
    fn remote_wait_truncates_results_while_retaining_all_target_states_in_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let ids = job_ids(8);
        let jobs = ids
            .iter()
            .enumerate()
            .map(|(index, id)| DeviceDelegationRow {
                reference_id: format!("ref-{index}"),
                root_task_id: "root".into(),
                device_id: format!("device-{index}"),
                profile_id: "temp".into(),
                device_path: vec![],
                job_id: Some(id.clone()),
                state: if index == 0 { "completed" } else { "running" }.into(),
                stop_status: "none".into(),
                can_stop: index != 0,
                result: (index == 0).then(|| "結果".repeat(10_000)),
            })
            .collect();
        let cursor = ids
            .iter()
            .map(|id| (id.clone(), "a".repeat(64)))
            .collect::<BTreeMap<_, _>>();
        let result = wait_result(
            Some(DeviceWaitResult {
                timed_out: false,
                jobs,
                cursor: cursor.clone(),
            }),
            &ids,
            None,
            &ToolTruncator,
            &ToolOutputConfig {
                max_lines: 4,
                max_bytes: 256,
                max_results: 8,
            },
            &output_paths(&temp),
        )
        .unwrap();
        assert_eq!(result.metadata["truncated"], true);
        assert_eq!(result.metadata["requested_job_ids"], json!(ids));
        assert_eq!(result.metadata["cursor"], json!(cursor));
        let metadata_jobs = result.metadata["jobs"].as_array().unwrap();
        assert_eq!(metadata_jobs.len(), 8);
        for (index, job) in metadata_jobs.iter().enumerate() {
            assert_eq!(job["job_id"], ids[index]);
            assert_eq!(
                job["state"],
                if index == 0 { "completed" } else { "running" }
            );
            assert_eq!(job["result_received"], index == 0);
            assert!(job.get("result").is_none());
        }
        let full: Value = serde_json::from_str(
            &std::fs::read_to_string(result.truncated_output_path.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(full["jobs"][0]["result"], "結果".repeat(10_000));
        assert!(result.output_text.contains("[output truncated]"));
        assert!(result._internal_file_lease.is_some());
        assert!(result.recorded_changes.is_empty());
    }

    #[test]
    fn remote_wait_interruption_output_preserves_requested_ids_without_remote_stop_claim() {
        let temp = tempfile::tempdir().unwrap();
        let ids = job_ids(1);
        let cursor = BTreeMap::from([(ids[0].clone(), "a".repeat(64))]);
        let result = wait_result(
            None,
            &ids,
            Some(&cursor),
            &ToolTruncator,
            &ResolvedConfig::default().tool_output,
            &output_paths(&temp),
        )
        .unwrap();
        let output: Value = serde_json::from_str(&result.output_text).unwrap();
        assert_eq!(output["interrupted"], true);
        assert_eq!(output["timed_out"], false);
        assert_eq!(output["requested_job_ids"], json!(ids));
        assert_eq!(output["jobs"], json!([]));
        assert_eq!(output["cursor"], json!(cursor));
        assert_eq!(result.metadata["cursor"], json!(cursor));
        assert_eq!(result.metadata["interrupted"], true);
        assert!(result.recorded_changes.is_empty());
    }
}
