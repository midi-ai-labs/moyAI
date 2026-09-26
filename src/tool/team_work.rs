//! One ordinary conversation can ask an authorized PC to do a bounded piece of work.
//! Hub remains the authority for project grants, target membership, and job state.
use std::io::Read;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::edit::CommittedFileMutation;
use crate::error::ToolError;
use crate::tool::context::{ToolContext, targets_configured_instruction_authority};
use crate::tool::registry::Tool;
use crate::tool::write_support::{delete_file_conditionally, write_bytes_file_conditionally};
use crate::tool::{
    PermissionRisk, ToolEffectClass, ToolEffectPolicy, ToolName, ToolResult, ToolSpec,
};
use crate::workspace::{AccessKind, PathGuard};

pub(crate) struct TeamPcsTool;
pub(crate) struct TeamUploadFileTool;
pub(crate) struct SharedUploadFileTool {
    pub project_id: String,
    pub job_id: String,
    pub attempt_id: String,
    pub generation: u64,
}
pub(crate) struct SharedJobArtifactsTool {
    pub project_id: String,
    pub parent_job_id: String,
    pub attempt_id: String,
    pub generation: u64,
}
pub(crate) struct SharedReadArtifactTool {
    pub project_id: String,
    pub parent_job_id: String,
    pub attempt_id: String,
    pub generation: u64,
}
pub(crate) struct SharedSaveArtifactTool {
    pub project_id: String,
    pub parent_job_id: String,
    pub attempt_id: String,
    pub generation: u64,
}

#[derive(Clone, Copy)]
struct SharedChildScope<'a> {
    project_id: &'a str,
    parent_job_id: &'a str,
    attempt_id: &'a str,
    generation: u64,
}
pub(crate) struct TeamDelegateTool;
pub(crate) struct TeamRetrySubmissionTool;
pub(crate) struct TeamWaitTool;
pub(crate) struct TeamReadArtifactTool;
pub(crate) struct TeamSaveArtifactTool;
pub(crate) struct TeamStopServiceTool;
pub(crate) struct TeamStopConversationTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PcsInput {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    pc_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateInput {
    project_id: String,
    environment_id: String,
    title: String,
    prompt: String,
    #[serde(default)]
    input_refs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadFileInput {
    project_id: String,
    path: Utf8PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedFileInput {
    path: Utf8PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedJobInput {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedReadArtifactInput {
    job_id: String,
    asset_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedSaveArtifactInput {
    job_id: String,
    asset_id: String,
    path: Utf8PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitInput {
    project_id: String,
    job_id: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArtifactInput {
    project_id: String,
    job_id: String,
    asset_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArtifactInput {
    project_id: String,
    job_id: String,
    asset_id: String,
    path: Utf8PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopServiceInput {
    service_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopConversationInput {
    project_id: String,
    conversation_id: String,
}

fn network(
    ctx: &ToolContext<'_>,
) -> Result<crate::device_network::DeviceNetworkService, ToolError> {
    ctx.services.store.device_network().ok_or_else(|| {
        ToolError::Message(
            "This session has no Hub device connection. Connect this PC to Hub first.".into(),
        )
    })
}

fn result(title: &str, value: Value) -> Result<ToolResult, ToolError> {
    Ok(ToolResult {
        title: title.into(),
        output_text: serde_json::to_string_pretty(&value)?,
        metadata: value,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    })
}

#[async_trait(?Send)]
impl Tool for TeamPcsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamPcs,
            effect: ToolEffectPolicy::read(),
            description: "List Hub project PCs this device may use for team work. Resolve a user's PC name here before delegating. Declared software capabilities are hints; confirm the program actually runs on that PC. Do not substitute another PC when the user explicitly named one.",
            input_schema: json!({
                "type":"object", "additionalProperties":false,
                "properties":{
                    "project_id":{"type":"string","description":"Optional Hub project ID. Omit to list all projects this device may submit to."},
                    "pc_name":{"type":"string","description":"Optional exact PC label to resolve."}
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: PcsInput = serde_json::from_value(raw_arguments)?;
        let value = network(&ctx)?
            .agent_environments(input.project_id.as_deref(), input.pc_name.as_deref())
            .await
            .map_err(ToolError::Message)?;
        result("チームで使えるPCを確認", value)
    }
}

#[async_trait(?Send)]
impl Tool for TeamUploadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamUploadFile,
            effect: ToolEffectPolicy::mutation(),
            description: "Upload one selected file (at most 8 MiB) from the current workspace to an authorized Hub project as an immutable input copy. This does not share the whole workspace or conversation. Pass the returned asset_id in team_delegate input_refs; verify the result before applying changes back locally.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["project_id","path"],
                "properties":{"project_id":{"type":"string"},"path":{"type":"string"}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: UploadFileInput = serde_json::from_value(raw_arguments)?;
        upload_file(&input.project_id, &input.path, None, ctx).await
    }
}

#[async_trait(?Send)]
impl Tool for SharedUploadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedUploadFile,
            effect: ToolEffectPolicy::mutation(),
            description: "Stage one file from this PC's bound project folder as an immutable Hub input (at most 8 MiB). The returned asset_id can be passed in shared_delegate input_refs. Only this selected file is copied; the child PC never reads this PC's folder directly.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["path"],"properties":{"path":{"type":"string"}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: SharedFileInput = serde_json::from_value(raw_arguments)?;
        ctx.services
            .store
            .session_repo()
            .require_active_shared_tool(
                &self.job_id,
                ctx.session.session.id,
                ctx.run_mutation_fence.turn_id(),
            )?;
        upload_file(
            &self.project_id,
            &input.path,
            Some((&self.job_id, &self.attempt_id, self.generation)),
            ctx,
        )
        .await
    }
}

async fn upload_file(
    project_id: &str,
    path: &Utf8PathBuf,
    shared_attempt: Option<(&str, &str, u64)>,
    mut ctx: ToolContext<'_>,
) -> Result<ToolResult, ToolError> {
    ctx.run_mutation_fence.assert_owned().await?;
    let resolved = crate::tool::internal_output::resolve_path(&ctx, path, AccessKind::Read).await?;
    let permission = resolved.permission();
    if shared_attempt.is_some() && !permission.inside_workspace {
        return Err(ToolError::Message(
            "Choose a file inside this PC's bound project folder".into(),
        ));
    }
    let absolute = permission.absolute.to_owned();
    let outside = !permission.inside_workspace && !permission.trusted_external;
    let admission = ctx
        .confirm_if_needed(
            AccessKind::Edit,
            format!("{} をチームのプロジェクトへ添付", absolute),
            vec![absolute],
            outside,
            ToolEffectClass::Mutation.permission_risks(),
        )
        .await?;
    admission.admit()?;
    let mut opened = resolved.into_read_file()?;
    let name = if shared_attempt.is_some() {
        opened.relative_to_root().as_str().replace('\\', "/")
    } else {
        opened
            .absolute()
            .file_name()
            .ok_or_else(|| ToolError::Message("File name is missing".into()))?
            .to_owned()
    };
    if name.is_empty() || name.len() > 512 {
        return Err(ToolError::Message(
            "The selected file name is too long".into(),
        ));
    }
    let before = opened.metadata()?;
    if !before.is_file() || before.len() > 8 * 1024 * 1024 {
        return Err(ToolError::Message(
            "The selected input must be one regular file of at most 8 MiB".into(),
        ));
    }
    let bytes = opened.with_file(|file| {
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    })?;
    let after = opened.metadata()?;
    if bytes.len() > 8 * 1024 * 1024
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(ToolError::Message(
            "The file changed during upload preparation; select it again".into(),
        ));
    }
    let network = network(&ctx)?;
    let value = if let Some((job_id, attempt_id, generation)) = shared_attempt {
        network
            .agent_upload_shared_input(
                &ctx.tool_call_id.to_string(),
                project_id,
                job_id,
                attempt_id,
                generation,
                &name,
                bytes,
                || {
                    admission.admit().map_err(|error| error.to_string())?;
                    ctx.run_mutation_fence
                        .begin_effect_commit()
                        .map_err(|error| error.to_string())
                },
            )
            .await
    } else {
        network
            .agent_upload_file(
                &ctx.tool_call_id.to_string(),
                project_id,
                &name,
                bytes,
                || {
                    admission.admit().map_err(|error| error.to_string())?;
                    ctx.run_mutation_fence
                        .begin_effect_commit()
                        .map_err(|error| error.to_string())
                },
            )
            .await
    }
    .map_err(ToolError::Message)?;
    result("別PCへ渡すファイルを保存", value)
}

#[async_trait(?Send)]
impl Tool for TeamDelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamDelegate,
            effect: ToolEffectPolicy::mutation(),
            description: "Send a bounded job from this ordinary conversation to an authorized Hub project PC returned by team_pcs. The Hub checks the grant again. Include only the necessary instructions and file references; local conversation history is not copied. The returned job ID is durable: use team_wait or inspect that job rather than resubmitting on uncertainty. Explicit user PC choices are binding.",
            input_schema: json!({
                "type":"object", "additionalProperties":false,
                "required":["project_id","environment_id","title","prompt"],
                "properties":{
                    "project_id":{"type":"string","description":"Hub project ID from team_pcs."},
                    "environment_id":{"type":"string","description":"Environment ID for the requested PC from team_pcs."},
                    "title":{"type":"string","minLength":1,"maxLength":256},
                    "prompt":{"type":"string","minLength":1,"maxLength":32768}
                    ,"input_refs":{"type":"array","maxItems":32,"uniqueItems":true,
                        "items":{"type":"string"},"description":"Immutable asset IDs returned by team_upload_file."}
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: DelegateInput = serde_json::from_value(raw_arguments)?;
        ctx.run_mutation_fence.assert_owned().await?;
        let admission = ctx
            .confirm_if_needed(
                AccessKind::Edit,
                format!("{} のPCへ「{}」を依頼", input.project_id, input.title),
                Vec::new(),
                false,
                ToolEffectClass::Mutation.permission_risks(),
            )
            .await?;
        let origin_turn_revision = ctx.run_mutation_fence.origin_turn_revision().await?;
        let network = network(&ctx)?;
        let value = network
            .agent_submit_job(
                &ctx.tool_call_id.to_string(),
                &ctx.session.session.id.to_string(),
                &ctx.run_mutation_fence.turn_id().to_string(),
                origin_turn_revision,
                &input.project_id,
                &input.environment_id,
                &input.title,
                &input.prompt,
                &input.input_refs,
                || {
                    admission.admit().map_err(|error| error.to_string())?;
                    ctx.run_mutation_fence
                        .begin_effect_commit()
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .map_err(ToolError::Message)?;
        result("PCへ仕事を依頼", value)
    }
}

#[async_trait(?Send)]
impl Tool for TeamRetrySubmissionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamRetrySubmission,
            effect: ToolEffectPolicy::mutation(),
            description: "When a team_delegate reply was uncertain, retry only the saved exact Hub submission for this conversation. This reuses its original request ID, target, prompt, and inputs; it does not create a new job. Never call team_delegate again for the same uncertain work before resolving its receipt.",
            input_schema: json!({"type":"object","additionalProperties":false,"properties":{}}),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        if raw_arguments != json!({}) {
            return Err(ToolError::Message(
                "team_retry_submission takes no arguments".into(),
            ));
        }
        ctx.run_mutation_fence.assert_owned().await?;
        let admission = ctx
            .confirm_if_needed(
                AccessKind::Edit,
                "未確認のPCへの依頼を同じ受付番号で再確認".into(),
                Vec::new(),
                false,
                ToolEffectClass::Mutation.permission_risks(),
            )
            .await?;
        let value = network(&ctx)?
            .agent_retry_submission(&ctx.session.session.id.to_string(), || {
                admission.admit().map_err(|error| error.to_string())?;
                ctx.run_mutation_fence
                    .begin_effect_commit()
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(ToolError::Message)?;
        result("未確認の依頼を同じ受付番号で確認", value)
    }
}

#[async_trait(?Send)]
impl Tool for TeamWaitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamWait,
            effect: ToolEffectPolicy::read(),
            description: "Wait for an existing Hub job from team_delegate without resubmitting it or consuming model calls. Completed jobs include authorized result file IDs; use team_read_artifact for small UTF-8 files. New user input interrupts the wait. If timed out or Hub is unavailable, keep the same job ID and check it again; unknown state is not completion.",
            input_schema: json!({
                "type":"object", "additionalProperties":false,
                "required":["project_id","job_id"],
                "properties":{
                    "project_id":{"type":"string"},
                    "job_id":{"type":"string"},
                    "timeout_ms":{"type":"integer","minimum":1000,"maximum":3600000,"default":600000}
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: WaitInput = serde_json::from_value(raw_arguments)?;
        let timeout_ms = input.timeout_ms.unwrap_or(600_000);
        if !(1_000..=3_600_000).contains(&timeout_ms) {
            return Err(ToolError::Message(
                "team_wait timeout_ms must be between 1000 and 3600000".into(),
            ));
        }
        let network = network(&ctx)?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if ctx.run_control.is_cancelled() {
                return Err(ToolError::RunInterrupted);
            }
            if ctx.run_mutation_fence.has_pending_turn_steer_input()? {
                return result(
                    "新しい指示で待機を中断",
                    json!({"interrupted":true,"job_id":input.job_id,
                    "message":"The remote job continues; reuse this job ID after handling the new input."}),
                );
            }
            let status = network
                .agent_job_status(&input.project_id, &input.job_id)
                .await;
            match status {
                Ok(value)
                    if matches!(
                        value["state"].as_str(),
                        Some("succeeded" | "failed" | "cancelled")
                    ) =>
                {
                    return result("PCの仕事が終了", value);
                }
                Ok(value) if Instant::now() >= deadline => {
                    return result("PCの仕事は継続中", json!({"timed_out":true,"job":value}));
                }
                Ok(_) => {}
                Err(error) => {
                    return result(
                        "PCの状態を確認できません",
                        json!({"job_id":input.job_id,
                        "project_id":input.project_id,"uncertain":true,"reason":error}),
                    );
                }
            }
            let wait =
                Duration::from_secs(3).min(deadline.saturating_duration_since(Instant::now()));
            let cancel = ctx.run_control.token();
            tokio::select! {
                _ = cancel.cancelled() => return Err(ToolError::RunInterrupted),
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }
}

#[async_trait(?Send)]
impl Tool for SharedJobArtifactsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedJobArtifacts,
            effect: ToolEffectPolicy::read(),
            description: "Inspect a completed child job in this Hub project and list its published artifact IDs. Use the job_id returned by shared_delegate; the Hub checks project access again.",
            input_schema: json!({"type":"object","additionalProperties":false,
                "required":["job_id"],"properties":{"job_id":{"type":"string"}}}),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: SharedJobInput = serde_json::from_value(raw_arguments)?;
        let value = network(&ctx)?
            .agent_shared_child_artifacts(
                &self.project_id,
                &self.parent_job_id,
                &self.attempt_id,
                self.generation,
                &input.job_id,
            )
            .await
            .map_err(ToolError::Message)?;
        result("別PCの仕事と成果を確認", value)
    }
}

#[async_trait(?Send)]
impl Tool for SharedReadArtifactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedReadArtifact,
            effect: ToolEffectPolicy::read(),
            description: "Read one UTF-8 child job artifact of at most 64 KiB in this Hub project. Supply the exact job_id and asset_id from shared_job_artifacts; Hub verifies access and the content hash.",
            input_schema: json!({"type":"object","additionalProperties":false,
                "required":["job_id","asset_id"],
                "properties":{"job_id":{"type":"string"},"asset_id":{"type":"string"}}}),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: SharedReadArtifactInput = serde_json::from_value(raw_arguments)?;
        let (asset, bytes) = network(&ctx)?
            .agent_download_shared_child_artifact(
                &self.project_id,
                &self.parent_job_id,
                &self.attempt_id,
                self.generation,
                &input.job_id,
                &input.asset_id,
                64 * 1024,
            )
            .await
            .map_err(ToolError::Message)?;
        let content = String::from_utf8(bytes).map_err(|_| {
            ToolError::Message(
                "This artifact is not UTF-8 text; use shared_save_artifact to save it".into(),
            )
        })?;
        result(
            "別PCの成果ファイルを確認",
            json!({"asset_id":asset.id,"project_id":self.project_id,"job_id":input.job_id,
                "name":asset.name,"sha256":asset.sha256,"byte_length":asset.byte_length,
                "content":content}),
        )
    }
}

#[async_trait(?Send)]
impl Tool for SharedSaveArtifactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedSaveArtifact,
            effect: ToolEffectPolicy::mutation(),
            description: "Save one child job artifact of at most 8 MiB into a new path in this PC's bound project folder. Supply the exact job_id and asset_id from shared_job_artifacts. Hub verifies the asset and its hash; an existing file is never overwritten.",
            input_schema: json!({"type":"object","additionalProperties":false,
                "required":["job_id","asset_id","path"],
                "properties":{"job_id":{"type":"string"},"asset_id":{"type":"string"},
                    "path":{"type":"string"}}}),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: SharedSaveArtifactInput = serde_json::from_value(raw_arguments)?;
        save_artifact(
            SaveArtifactInput {
                project_id: self.project_id.clone(),
                job_id: input.job_id,
                asset_id: input.asset_id,
                path: input.path,
            },
            ctx,
            Some(SharedChildScope {
                project_id: &self.project_id,
                parent_job_id: &self.parent_job_id,
                attempt_id: &self.attempt_id,
                generation: self.generation,
            }),
        )
        .await
    }
}

#[async_trait(?Send)]
impl Tool for TeamReadArtifactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamReadArtifact,
            effect: ToolEffectPolicy::read(),
            description: "Read a UTF-8 result file of at most 64 KiB from an exact team job, using an asset_id returned by team_wait. Hub rechecks project access and the content hash. This does not write the local workspace; use team_save_artifact for binary or larger files.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["project_id","job_id","asset_id"],
                "properties":{"project_id":{"type":"string"},"job_id":{"type":"string"},
                    "asset_id":{"type":"string"}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: ReadArtifactInput = serde_json::from_value(raw_arguments)?;
        let value = network(&ctx)?
            .agent_read_artifact_text(&input.project_id, &input.job_id, &input.asset_id)
            .await
            .map_err(ToolError::Message)?;
        result("別PCの成果ファイルを確認", value)
    }
}

#[async_trait(?Send)]
impl Tool for TeamSaveArtifactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamSaveArtifact,
            effect: ToolEffectPolicy::mutation(),
            description: "Save one exact team job artifact (up to 8 MiB, including binary files) into this conversation's selected local workspace. Supply a new path whose parent already exists. The Hub project, job, asset version and SHA-256 are checked before the local write. An existing destination is never overwritten; inspect or choose another path if it has changed.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["project_id","job_id","asset_id","path"],
                "properties":{"project_id":{"type":"string"},"job_id":{"type":"string"},
                    "asset_id":{"type":"string"},
                    "path":{"type":"string","description":"New file path inside the current local workspace."}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: SaveArtifactInput = serde_json::from_value(raw_arguments)?;
        save_artifact(input, ctx, None).await
    }
}

async fn save_artifact(
    input: SaveArtifactInput,
    mut ctx: ToolContext<'_>,
    shared_child: Option<SharedChildScope<'_>>,
) -> Result<ToolResult, ToolError> {
    let guarded = PathGuard::require_path(ctx.workspace, &input.path, AccessKind::Edit)?;
    if !guarded.inside_workspace {
        return Err(ToolError::Message(
            "Choose a destination inside the selected local workspace".into(),
        ));
    }
    let path = guarded.absolute.clone();
    let mut risks = ToolEffectClass::Mutation.permission_risks();
    if PathGuard::targets_protected_workspace_authority(&ctx.workspace.root, &guarded)
        || targets_configured_instruction_authority(ctx.config, ctx.workspace, &path)
    {
        risks.push(PermissionRisk::ProtectedWorkspaceAuthority);
    }
    let admission = ctx
        .confirm_if_needed(
            AccessKind::Edit,
            format!("別PCの成果 {} を {} に保存", input.asset_id, path),
            vec![path.clone()],
            false,
            risks,
        )
        .await?;
    ctx.run_mutation_fence.assert_owned().await?;
    let network = network(&ctx)?;
    let (asset, bytes) = if let Some(scope) = shared_child {
        network
            .agent_download_shared_child_artifact(
                scope.project_id,
                scope.parent_job_id,
                scope.attempt_id,
                scope.generation,
                &input.job_id,
                &input.asset_id,
                8 * 1024 * 1024,
            )
            .await
    } else {
        network
            .agent_download_artifact(&input.project_id, &input.job_id, &input.asset_id)
            .await
    }
    .map_err(ToolError::Message)?;
    let edit_safety = ctx.services.edit_safety.clone();
    let session_id = ctx.session.session.id;
    let fence = ctx.run_mutation_fence.clone();
    edit_safety
        .with_file_lock(&path, async {
            PathGuard::revalidate(&guarded)?;
            edit_safety.assert_fresh_create(session_id, &path)?;
            edit_safety.assert_path_unchanged(&path, None)?;
            fence.assert_owned().await?;
            admission.admit()?;
            let _effect_commit = fence.begin_effect_commit()?;
            let identity = write_bytes_file_conditionally(&guarded, &bytes, None, |_| Ok(()))?;
            if let Err(error) = edit_safety.sync_file_mutations(
                session_id,
                &[CommittedFileMutation::present(
                    path.clone(),
                    identity.clone(),
                )],
                8 * 1024 * 1024,
            ) {
                delete_file_conditionally(&guarded, &identity)?;
                return Err(ToolError::from(error));
            }
            Ok::<(), ToolError>(())
        })
        .await?;
    result(
        "別PCの成果をワークスペースへ保存",
        json!({"project_id":input.project_id,"job_id":input.job_id,
                "asset_id":asset.id,"asset_version":asset.version,
                "source_name":asset.name,"path":path,"sha256":asset.sha256,
                "byte_length":asset.byte_length,"saved":true}),
    )
}

#[async_trait(?Send)]
impl Tool for TeamStopServiceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamStopService,
            effect: ToolEffectPolicy::mutation(),
            description: "Request an exact retained server or preview to stop. Use a service_id reported by team_wait for the current job. Hub checks project authority; the response means stop was requested, not that the process has ended. Check the job again for confirmation.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["service_id"],
                "properties":{"service_id":{"type":"string"}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: StopServiceInput = serde_json::from_value(raw_arguments)?;
        ctx.run_mutation_fence.assert_owned().await?;
        let admission = ctx
            .confirm_if_needed(
                AccessKind::Edit,
                "別PCで起動中のサーバーの停止を依頼".into(),
                Vec::new(),
                false,
                ToolEffectClass::Mutation.permission_risks(),
            )
            .await?;
        let network = network(&ctx)?;
        let value = network
            .agent_stop_service(&input.service_id, || {
                admission.admit().map_err(|error| error.to_string())?;
                ctx.run_mutation_fence
                    .begin_effect_commit()
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(ToolError::Message)?;
        result("別PCのサーバーへ停止を依頼", value)
    }
}

#[async_trait(?Send)]
impl Tool for TeamStopConversationTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::TeamStopConversation,
            effect: ToolEffectPolicy::mutation(),
            description: "Request stop for every retained service already associated with an exact Hub conversation. Use its project_id and conversation_id from a team job. Hub verifies authority over all affected services. This acknowledges the stop request, not process exit; check each job for confirmation. A retry of an uncertain request keeps the same durable request ID and cannot stop services started later.",
            input_schema: json!({
                "type":"object","additionalProperties":false,
                "required":["project_id","conversation_id"],
                "properties":{"project_id":{"type":"string"},"conversation_id":{"type":"string"}}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: StopConversationInput = serde_json::from_value(raw_arguments)?;
        ctx.run_mutation_fence.assert_owned().await?;
        let admission = ctx
            .confirm_if_needed(
                AccessKind::Edit,
                "この会話で起動した別PCのサーバーをすべて停止".into(),
                Vec::new(),
                false,
                ToolEffectClass::Mutation.permission_risks(),
            )
            .await?;
        let value = network(&ctx)?
            .agent_stop_conversation(
                &ctx.tool_call_id.to_string(),
                &input.project_id,
                &input.conversation_id,
                || {
                    admission.admit().map_err(|error| error.to_string())?;
                    ctx.run_mutation_fence
                        .begin_effect_commit()
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .map_err(ToolError::Message)?;
        result("会話で起動した別PCのサーバーへ停止を依頼", value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_tools_have_distinct_canonical_names_and_mutation_boundary() {
        for (name, tool) in [
            ("team_pcs", TeamPcsTool.spec()),
            ("team_upload_file", TeamUploadFileTool.spec()),
            ("team_delegate", TeamDelegateTool.spec()),
            ("team_retry_submission", TeamRetrySubmissionTool.spec()),
            ("team_wait", TeamWaitTool.spec()),
            ("team_read_artifact", TeamReadArtifactTool.spec()),
            ("team_save_artifact", TeamSaveArtifactTool.spec()),
            ("team_stop_service", TeamStopServiceTool.spec()),
            ("team_stop_conversation", TeamStopConversationTool.spec()),
        ] {
            assert_eq!(tool.name.to_string(), name);
            assert_eq!(ToolName::parse(name), tool.name);
        }
        assert_eq!(TeamDelegateTool.spec().effect, ToolEffectPolicy::mutation());
        assert_eq!(
            TeamStopConversationTool.spec().effect,
            ToolEffectPolicy::mutation()
        );
    }

    #[test]
    fn shared_file_tools_take_only_job_and_file_references_from_the_model() {
        let tools = [
            (
                "shared_upload_file",
                SharedUploadFileTool {
                    project_id: "project-a".into(),
                    job_id: "job-a".into(),
                    attempt_id: "attempt-a".into(),
                    generation: 1,
                }
                .spec(),
            ),
            (
                "shared_job_artifacts",
                SharedJobArtifactsTool {
                    project_id: "project-a".into(),
                    parent_job_id: "job-a".into(),
                    attempt_id: "attempt-a".into(),
                    generation: 1,
                }
                .spec(),
            ),
            (
                "shared_read_artifact",
                SharedReadArtifactTool {
                    project_id: "project-a".into(),
                    parent_job_id: "job-a".into(),
                    attempt_id: "attempt-a".into(),
                    generation: 1,
                }
                .spec(),
            ),
            (
                "shared_save_artifact",
                SharedSaveArtifactTool {
                    project_id: "project-a".into(),
                    parent_job_id: "job-a".into(),
                    attempt_id: "attempt-a".into(),
                    generation: 1,
                }
                .spec(),
            ),
        ];
        for (name, spec) in tools {
            assert_eq!(spec.name.to_string(), name);
            assert_eq!(ToolName::parse(name), spec.name);
            assert!(spec.input_schema["properties"].get("project_id").is_none());
            assert_eq!(spec.input_schema["additionalProperties"], false);
        }
    }
}
