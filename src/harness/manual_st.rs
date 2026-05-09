use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::str::FromStr;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::agent::state::{ActiveWorkContract, active_work_contract_for_history_items};
use crate::app::{App, AppBootstrap, AppCommand, RunRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode};
use crate::config::model::{PartialPermissionsConfig, PartialResolvedConfig};
use crate::config::{AccessMode, ShellFamily};
use crate::error::{CliPromptError, CliRenderError};
use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload, ProtocolEventStore};
use crate::runtime::SystemClock;
use crate::session::SessionRepository;
use crate::session::{
    EditorContext, MessageRole, PromptDispatchPart, RunEvent, RunSummary, SessionId, SessionStatus,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::PermissionRequest;

const FIXTURE_VERSION: &str = "manual_st_route_runner.v1";
const DEFAULT_PROVIDER_BASE_URL: &str = "http://192.168.10.103:1234";
const DEFAULT_MODEL_ID: &str = "qwen/qwen3.6-35b-a3b";
const MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE: usize = 6;

#[derive(Debug, Clone)]
pub struct ManualStRouteRunConfig {
    pub route: ManualStRouteKind,
    pub output_root: Option<Utf8PathBuf>,
    pub preflight_report: Utf8PathBuf,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub max_turn_seconds: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualStRouteKind {
    RequiredCore,
    RequiredVision,
    RequiredVisionFull,
    TargetedSupport,
    ExtendedCase4,
    ExtendedCase5,
    ExtendedCase7,
    ProbeCase6,
}

impl ManualStRouteKind {
    pub fn route_id(self) -> &'static str {
        match self {
            Self::RequiredCore => "required_core_route_a",
            Self::RequiredVision => "required_vision_route_b",
            Self::RequiredVisionFull => "required_vision_route_b_full",
            Self::TargetedSupport => "targeted_support_case2b",
            Self::ExtendedCase4 => "extended_route_c_case4",
            Self::ExtendedCase5 => "extended_route_d_case5",
            Self::ExtendedCase7 => "extended_route_e_case7",
            Self::ProbeCase6 => "probe_route_f_case6",
        }
    }

    pub fn route_type(self) -> &'static str {
        match self {
            Self::RequiredCore => "required_core",
            Self::RequiredVision | Self::RequiredVisionFull => "required_vision",
            Self::TargetedSupport => "targeted_support",
            Self::ExtendedCase4 | Self::ExtendedCase5 | Self::ExtendedCase7 => "extended",
            Self::ProbeCase6 => "probe",
        }
    }

    pub fn case_ids(self) -> Vec<&'static str> {
        match self {
            Self::RequiredCore => vec!["case1", "case3"],
            Self::RequiredVision => vec!["case2c"],
            Self::RequiredVisionFull => vec!["case2a", "case2c"],
            Self::TargetedSupport => vec!["case2b"],
            Self::ExtendedCase4 => vec!["case4"],
            Self::ExtendedCase5 => vec!["case5"],
            Self::ExtendedCase7 => vec!["case7"],
            Self::ProbeCase6 => vec!["case6"],
        }
    }
}

impl FromStr for ManualStRouteKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "required-core" | "core" | "route-a" => Ok(Self::RequiredCore),
            "required-vision" | "vision" | "case2c" => Ok(Self::RequiredVision),
            "required-vision-full" | "vision-full" | "case2a-case2c" => {
                Ok(Self::RequiredVisionFull)
            }
            "targeted-support" | "case2b" => Ok(Self::TargetedSupport),
            "extended-case4" | "case4" | "route-c" => Ok(Self::ExtendedCase4),
            "extended-case5" | "case5" | "route-d" => Ok(Self::ExtendedCase5),
            "extended-case7" | "case7" | "route-e" => Ok(Self::ExtendedCase7),
            "probe-case6" | "case6" | "route-f" => Ok(Self::ProbeCase6),
            other => Err(format!("unknown manual ST route `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualStRoutePlan {
    pub route_id: String,
    pub route_type: String,
    pub case_ids: Vec<String>,
    pub required_artifacts: Vec<String>,
}

pub fn manual_st_route_plan(route: ManualStRouteKind) -> ManualStRoutePlan {
    ManualStRoutePlan {
        route_id: route.route_id().to_string(),
        route_type: route.route_type().to_string(),
        case_ids: route.case_ids().into_iter().map(str::to_string).collect(),
        required_artifacts: vec![
            "route_manifest.json".to_string(),
            "verification_command_log.json".to_string(),
            "workspace_diff_manifest.json".to_string(),
            "result.json".to_string(),
            "preflight_report.json".to_string(),
            "timeout_classification.json".to_string(),
        ],
    }
}

pub async fn run_manual_st_route(
    config: ManualStRouteRunConfig,
) -> Result<ManualStRouteResult, String> {
    validate_preflight_report(&config.preflight_report)?;
    let route_root = config
        .output_root
        .clone()
        .unwrap_or_else(|| default_manual_st_route_root(config.route, SystemClock::now_ms()));
    reset_route_output_root(&route_root, config.route)?;
    fs::create_dir_all(route_root.as_std_path()).map_err(|error| {
        format!("failed to create manual ST route root `{route_root}`: {error}")
    })?;
    fs::copy(
        config.preflight_report.as_std_path(),
        route_root.join("preflight_report.json").as_std_path(),
    )
    .map_err(|error| format!("failed to copy preflight report: {error}"))?;

    let mut result = ManualStRouteResult::started(&config, route_root.clone());
    if config.dry_run {
        result.route_level_verdict = RouteVerdict::NotRun;
        result.stop_reason = Some("dry_run requested; no live LLM route was executed".to_string());
        write_route_artifacts(&route_root, &result, Vec::new(), WorkspaceDiff::empty())?;
        return Ok(result);
    }
    result.route_level_verdict = RouteVerdict::Running;
    result.stop_reason =
        Some("route started; terminal route verdict has not been materialized yet".to_string());
    write_route_artifacts(&route_root, &result, Vec::new(), WorkspaceDiff::empty())?;
    result.stop_reason = None;

    let mut verification_commands = Vec::new();
    let mut last_case_workspace = None;
    let mut previous_case_workspace = None;
    let mut reached_expected_artifacts = Vec::new();

    for case_id in config.route.case_ids() {
        let case_spec = ManualStCaseSpec::load(case_id)?;
        let case_expected_artifacts = case_spec.expected_artifacts.clone();
        reached_expected_artifacts.extend(case_expected_artifacts.clone());
        let case_root = route_root.join(case_id);
        let workspace = case_root.join("workspace");
        let data_dir = case_root.join("data");
        fs::create_dir_all(workspace.as_std_path())
            .map_err(|error| format!("failed to create workspace `{workspace}`: {error}"))?;
        fs::create_dir_all(data_dir.as_std_path())
            .map_err(|error| format!("failed to create data dir `{data_dir}`: {error}"))?;

        prepare_case_workspace(
            case_id,
            &case_spec,
            previous_case_workspace.as_ref(),
            &workspace,
        )?;
        let case_result = run_case(&config, &case_spec, &case_root, &workspace, &data_dir).await?;
        verification_commands.extend(case_result.verification_commands.clone());
        result.session_ids.extend(case_result.session_ids.clone());
        result.case_results.push(case_result.clone());
        last_case_workspace = Some(workspace.clone());
        if !matches!(case_result.verdict, RouteVerdict::Pass) {
            result.route_level_verdict = case_result.verdict;
            result.stop_reason = case_result
                .stop_reason
                .clone()
                .or_else(|| Some(format!("{case_id} did not pass; route stopped fail-stop")));
        }
        if case_result.timeout_observed {
            write_timeout_classification(&route_root, true, false)?;
        }
        propagate_case_timeout_classification(&route_root, &case_root, case_result.verdict)?;
        let partial_workspace_diff =
            WorkspaceDiff::from_workspace(&workspace, &case_expected_artifacts)?;
        write_route_artifacts(
            &route_root,
            &result,
            verification_commands.clone(),
            partial_workspace_diff,
        )?;

        if !matches!(case_result.verdict, RouteVerdict::Pass) {
            break;
        }
        previous_case_workspace = Some(workspace);
    }

    if result.case_results.len() == result.case_ids.len()
        && result
            .case_results
            .iter()
            .all(|case| matches!(case.verdict, RouteVerdict::Pass))
    {
        result.route_level_verdict = RouteVerdict::Pass;
    } else if !matches!(
        result.route_level_verdict,
        RouteVerdict::Fail | RouteVerdict::Blocked
    ) {
        result.route_level_verdict = RouteVerdict::Fail;
    }

    result.completed_at = timestamp();
    let workspace_diff = last_case_workspace
        .as_ref()
        .map(|workspace| WorkspaceDiff::from_workspace(workspace, &reached_expected_artifacts))
        .transpose()?
        .unwrap_or_else(WorkspaceDiff::empty);
    if result.case_results.iter().any(|case| case.timeout_observed) {
        write_timeout_classification(&route_root, true, false)?;
    }
    write_route_artifacts(&route_root, &result, verification_commands, workspace_diff)?;
    Ok(result)
}

fn reset_route_output_root(route_root: &Utf8Path, route: ManualStRouteKind) -> Result<(), String> {
    if !route_root.exists() {
        return Ok(());
    }
    for artifact in manual_st_route_plan(route).required_artifacts {
        let path = route_root.join(artifact);
        if path.exists() {
            fs::remove_file(path.as_std_path()).map_err(|error| {
                format!("failed to remove stale route artifact `{path}`: {error}")
            })?;
        }
    }
    for case_id in route.case_ids() {
        let case_root = route_root.join(case_id);
        if case_root.exists() {
            fs::remove_dir_all(case_root.as_std_path()).map_err(|error| {
                format!("failed to remove stale case artifact `{case_root}`: {error}")
            })?;
        }
    }
    Ok(())
}

fn propagate_case_timeout_classification(
    route_root: &Utf8Path,
    case_root: &Utf8Path,
    verdict: RouteVerdict,
) -> Result<(), String> {
    let case_timeout_classification = case_root.join("timeout_classification.json");
    if case_timeout_classification.exists() && !matches!(verdict, RouteVerdict::Pass) {
        fs::copy(
            case_timeout_classification.as_std_path(),
            route_root.join("timeout_classification.json").as_std_path(),
        )
        .map_err(|error| format!("failed to propagate case timeout classification: {error}"))?;
    }
    Ok(())
}

async fn run_case(
    config: &ManualStRouteRunConfig,
    case_spec: &ManualStCaseSpec,
    case_root: &Utf8Path,
    workspace: &Utf8Path,
    data_dir: &Utf8Path,
) -> Result<ManualStCaseResult, String> {
    let store = open_case_store(data_dir)?;
    let app =
        AppBootstrap::rebuild_for_directory_as_workspace_root(workspace, StoreBundle::new(store))
            .await
            .map_err(|error| format!("failed to build app for `{}`: {error}", case_spec.case_id))?;
    let mut renderer = RecordingRenderer::default();
    let mut prompt = HarnessConfirmationPrompt;
    let mut session_id = None;
    let mut verification_commands = Vec::new();
    let mut stop_reason = None;
    let mut verdict = RouteVerdict::Pass;
    let mut timeout_observed = false;
    let mut closeout_evidence = None;

    'stages: for (stage_index, stage) in case_spec.stages.iter().enumerate() {
        let mut stage_prompt = stage.prompt.clone();
        let mut closeout_continuation_turns = 0usize;
        let mut closeout_budget = CloseoutContinuationBudget::default();
        loop {
            let continuation = manual_st_stage_session_continuation(session_id);
            let request = RunRequest {
                prompt: stage_prompt.clone(),
                session_id: continuation.session_id,
                continue_last: continuation.continue_last,
                title: Some(format!("manual ST {} {}", case_spec.case_id, stage.label)),
                cwd: workspace.to_path_buf(),
                model: config
                    .model_override
                    .clone()
                    .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string()),
                base_url: config
                    .base_url_override
                    .clone()
                    .unwrap_or_else(|| DEFAULT_PROVIDER_BASE_URL.to_string()),
                config_override: Some(PartialResolvedConfig {
                    permissions: Some(PartialPermissionsConfig {
                        access_mode: Some(AccessMode::FullAccess),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                output_mode: OutputMode::Json,
                show_reasoning: false,
                prompt_dispatch: Some(PromptDispatchPart::raw(&stage_prompt)),
                editor_context: Some(EditorContext {
                    active_file: None,
                    visible_files: Vec::new(),
                    open_tabs: Vec::new(),
                    shell_family: if cfg!(windows) {
                        ShellFamily::PowerShell
                    } else {
                        ShellFamily::Bash
                    },
                    current_time_ms: SystemClock::now_ms(),
                }),
                review_request: None,
                image_paths: image_paths_for_closeout_attempt(
                    &case_spec.case_id,
                    closeout_continuation_turns,
                ),
            };
            let stage_result = tokio::time::timeout(
                Duration::from_secs(config.max_turn_seconds),
                app.run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt),
            )
            .await;
            let summary = match stage_result {
                Ok(Ok(summary)) => summary,
                Ok(Err(error)) => {
                    if session_id.is_none() {
                        session_id = latest_session_id_for_case(&app).await;
                    }
                    verdict = RouteVerdict::Fail;
                    let reason = format!(
                        "{} {} failed before completion: {error}",
                        case_spec.case_id, stage.label
                    );
                    if is_provider_stream_stall_reason(&reason)
                        || is_provider_transport_stream_error_reason(&reason)
                    {
                        write_timeout_classification_with_reason(
                            case_root,
                            false,
                            true,
                            Some(&reason),
                        )?;
                    }
                    stop_reason = Some(reason);
                    break 'stages;
                }
                Err(_) => {
                    if session_id.is_none() {
                        session_id = latest_session_id_for_case(&app).await;
                    }
                    verdict = RouteVerdict::Fail;
                    timeout_observed = true;
                    stop_reason = Some(format!(
                        "{} {} exceeded max_turn_seconds={}",
                        case_spec.case_id, stage.label, config.max_turn_seconds
                    ));
                    write_timeout_classification_with_reason(case_root, true, false, None)?;
                    break 'stages;
                }
            };
            session_id = Some(summary.session_id);
            if summary.status != SessionStatus::Completed {
                verdict = RouteVerdict::Fail;
                let runtime_failure = latest_session_failed_message(&renderer.events);
                let reason = format!(
                    "{} {} ended with session status {:?}",
                    case_spec.case_id, stage.label, summary.status
                );
                let reason = runtime_failure
                    .map(|message| format!("{reason}: {message}"))
                    .unwrap_or(reason);
                write_timeout_classification_with_reason(case_root, false, true, Some(&reason))?;
                stop_reason = Some(reason);
                break 'stages;
            }

            for command in &stage.verification_commands {
                let verification =
                    run_verification_command(command, workspace, &case_spec.case_id).await?;
                verification_commands.push(verification);
            }

            let actual_files_after_stage = list_workspace_files(workspace)?;
            let closeout = classify_manual_st_closeout(
                &app,
                &summary,
                &case_spec.expected_artifacts,
                &actual_files_after_stage,
                &verification_commands,
            )
            .await?;
            if closeout.closeout_class == ManualStCloseoutClass::CleanCloseout {
                closeout_evidence = Some(closeout);
                write_stage_events(case_root, stage_index + 1, &renderer.events)?;
                renderer.events.clear();
                continue 'stages;
            }

            if let Some(closeout_attempt) = closeout_budget.next_attempt(&closeout) {
                closeout_continuation_turns += 1;
                stage_prompt = build_closeout_continuation_prompt(
                    &case_spec.case_id,
                    &stage.label,
                    closeout_attempt,
                    &closeout,
                );
                closeout_evidence = Some(closeout);
                continue;
            }

            verdict = RouteVerdict::Fail;
            stop_reason = Some(format!(
                "{} {} closeout classified as {:?}: {}",
                case_spec.case_id,
                stage.label,
                closeout.closeout_class,
                closeout.diagnostics.join("; ")
            ));
            closeout_evidence = Some(closeout);
            break 'stages;
        }
    }

    let actual_files = list_workspace_files(workspace)?;
    for expected in &case_spec.expected_artifacts {
        if !actual_files.contains(expected) {
            verdict = RouteVerdict::Fail;
            stop_reason.get_or_insert_with(|| {
                format!(
                    "{} missing expected artifact `{expected}`",
                    case_spec.case_id
                )
            });
        }
    }

    Ok(ManualStCaseResult {
        case_id: case_spec.case_id.clone(),
        verdict,
        session_ids: session_id.into_iter().collect(),
        expected_artifacts: case_spec.expected_artifacts.clone(),
        actual_files,
        verification_commands,
        closeout_evidence,
        stop_reason,
        timeout_observed,
    })
}

async fn latest_session_id_for_case(app: &App) -> Option<SessionId> {
    app.store
        .session_repo()
        .latest_session(app.workspace.project_id)
        .await
        .ok()
        .flatten()
        .map(|session| session.id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManualStStageContinuation {
    session_id: Option<SessionId>,
    continue_last: bool,
}

fn manual_st_stage_session_continuation(
    previous_session_id: Option<SessionId>,
) -> ManualStStageContinuation {
    ManualStStageContinuation {
        session_id: previous_session_id,
        continue_last: false,
    }
}

pub(crate) fn multistage_continuation_uses_explicit_session_without_continue_last_fixture_passes()
-> bool {
    let first = manual_st_stage_session_continuation(None);
    let session_id = SessionId::new();
    let second = manual_st_stage_session_continuation(Some(session_id));

    first.session_id.is_none()
        && !first.continue_last
        && second.session_id == Some(session_id)
        && !second.continue_last
}

async fn classify_manual_st_closeout(
    app: &App,
    summary: &RunSummary,
    expected_artifacts: &[String],
    actual_files: &[String],
    verification_commands: &[VerificationCommandEvidence],
) -> Result<ManualStCloseoutEvidence, String> {
    let session_id = summary.session_id;
    let session = app
        .store
        .session_repo()
        .get_session(session_id)
        .await
        .map_err(|error| format!("failed to load session `{session_id}`: {error}"))?;
    let state = app
        .store
        .session_repo()
        .get_state(session_id)
        .await
        .map_err(|error| format!("failed to load session state `{session_id}`: {error}"))?;
    let todos = app
        .store
        .session_repo()
        .list_todos(session_id)
        .await
        .map_err(|error| format!("failed to load todos for `{session_id}`: {error}"))?;
    let history_items = app
        .store
        .protocol_event_store()
        .list_history_items_for_session(session_id)
        .map_err(|error| format!("failed to load protocol history for `{session_id}`: {error}"))?;
    let active_work =
        active_work_contract_for_history_items(&session, &history_items, &state, &todos);
    let mut evidence = classify_manual_st_closeout_from_evidence(
        summary.status == SessionStatus::Completed,
        latest_final_assistant_text(&history_items),
        active_work.as_ref(),
        expected_artifacts,
        actual_files,
        verification_commands,
    );
    let repair_targets = repair_targets_from_active_work(active_work.as_ref(), expected_artifacts);
    if !repair_targets.is_empty() {
        evidence.repair_targets = repair_targets;
    }
    Ok(evidence)
}

fn classify_manual_st_closeout_from_evidence(
    runtime_completed: bool,
    final_assistant_message: Option<String>,
    active_work: Option<&ActiveWorkContract>,
    expected_artifacts: &[String],
    actual_files: &[String],
    verification_commands: &[VerificationCommandEvidence],
) -> ManualStCloseoutEvidence {
    let missing_artifacts = expected_artifacts
        .iter()
        .filter(|artifact| !actual_files.contains(artifact))
        .cloned()
        .collect::<Vec<_>>();
    let latest_verification = latest_verification_evidence_by_command(verification_commands);
    let verification_passed = latest_verification
        .iter()
        .filter(|evidence| evidence.exit_code == Some(0))
        .map(|evidence| evidence.command.clone())
        .collect::<Vec<_>>();
    let verification_failed = latest_verification
        .iter()
        .filter(|evidence| evidence.exit_code != Some(0) && evidence.required)
        .map(|evidence| evidence.command.clone())
        .collect::<Vec<_>>();
    let verification_failure_evidence = latest_verification
        .iter()
        .filter(|evidence| evidence.exit_code != Some(0) && evidence.required)
        .map(|evidence| render_verification_failure_evidence(evidence))
        .collect::<Vec<_>>();
    let (open_obligations, verification_required) =
        open_obligations_from_active_work(active_work, &verification_passed);
    let mut diagnostics = Vec::new();
    if !runtime_completed {
        diagnostics.push("runtime did not complete its final assistant item lifecycle".to_string());
    }
    if !missing_artifacts.is_empty() {
        diagnostics.push(format!(
            "expected artifacts are missing: {}",
            missing_artifacts.join(", ")
        ));
    }
    if !open_obligations.is_empty() {
        diagnostics.push(format!(
            "typed open obligations remain: {}",
            open_obligations.join("; ")
        ));
    }
    if !verification_required.is_empty() {
        diagnostics.push(format!(
            "required verification evidence is missing: {}",
            verification_required.join(", ")
        ));
    }
    if !verification_failed.is_empty() {
        diagnostics.push(format!(
            "required verification failed: {}",
            verification_failed.join(", ")
        ));
    }

    let final_text = final_assistant_message.as_deref().unwrap_or_default();
    let has_open_obligation = !missing_artifacts.is_empty() || !open_obligations.is_empty();
    let closeout_class = if !runtime_completed {
        ManualStCloseoutClass::RuntimeDidNotComplete
    } else if has_open_obligation && final_message_promises_future_work(final_text) {
        ManualStCloseoutClass::ContinuationPromised
    } else if has_open_obligation && final_message_claims_completion(final_text) {
        ManualStCloseoutClass::EvidenceContradiction
    } else if has_open_obligation {
        ManualStCloseoutClass::IncompleteOpenObligation
    } else if !verification_failed.is_empty() || !verification_required.is_empty() {
        ManualStCloseoutClass::VerificationRequired
    } else {
        ManualStCloseoutClass::CleanCloseout
    };

    ManualStCloseoutEvidence {
        runtime_completed,
        closeout_class,
        final_assistant_message,
        open_obligations,
        expected_artifacts: expected_artifacts.to_vec(),
        missing_artifacts,
        verification_required,
        verification_passed,
        verification_failed,
        verification_failure_evidence,
        repair_targets: repair_targets_from_active_work(active_work, expected_artifacts),
        diagnostics,
    }
}

fn latest_verification_evidence_by_command(
    verification_commands: &[VerificationCommandEvidence],
) -> Vec<&VerificationCommandEvidence> {
    let mut latest = Vec::<(String, &VerificationCommandEvidence)>::new();
    for evidence in verification_commands {
        let normalized = normalize_command_for_route_evidence(&evidence.command);
        if let Some((_, existing)) = latest
            .iter_mut()
            .find(|(command, _)| command.as_str() == normalized.as_str())
        {
            *existing = evidence;
        } else {
            latest.push((normalized, evidence));
        }
    }
    latest
        .into_iter()
        .map(|(_, evidence)| evidence)
        .collect::<Vec<_>>()
}

fn should_continue_after_closeout(closeout: &ManualStCloseoutEvidence) -> bool {
    closeout.runtime_completed
        && matches!(
            closeout.closeout_class,
            ManualStCloseoutClass::ContinuationPromised
                | ManualStCloseoutClass::IncompleteOpenObligation
                | ManualStCloseoutClass::EvidenceContradiction
                | ManualStCloseoutClass::VerificationRequired
        )
}

#[derive(Default)]
struct CloseoutContinuationBudget {
    attempts_by_signature: BTreeMap<String, usize>,
}

impl CloseoutContinuationBudget {
    fn next_attempt(&mut self, closeout: &ManualStCloseoutEvidence) -> Option<usize> {
        if !should_continue_after_closeout(closeout) {
            return None;
        }
        let signature = closeout_continuation_signature(closeout)?;
        let attempts = self.attempts_by_signature.entry(signature).or_insert(0);
        if *attempts >= MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE {
            return None;
        }
        *attempts += 1;
        Some(*attempts)
    }
}

fn closeout_continuation_signature(closeout: &ManualStCloseoutEvidence) -> Option<String> {
    let kind = closeout_continuation_kind(closeout)?;
    let mut parts = vec![kind.to_string()];
    match kind {
        "verification_failed" => {
            parts.extend(closeout.verification_failed.iter().cloned());
            parts.extend(closeout.repair_targets.iter().cloned());
            parts.extend(closeout.verification_failure_evidence.iter().cloned());
        }
        "verification_missing" => {
            parts.extend(closeout.verification_required.iter().cloned());
        }
        "open_obligation" => {
            parts.extend(closeout.open_obligations.iter().cloned());
            parts.extend(closeout.missing_artifacts.iter().cloned());
        }
        "evidence_contradiction" => {
            parts.extend(closeout.open_obligations.iter().cloned());
            parts.extend(closeout.missing_artifacts.iter().cloned());
        }
        _ => {}
    }
    Some(parts.join("|"))
}

fn closeout_continuation_kind(closeout: &ManualStCloseoutEvidence) -> Option<&'static str> {
    if !closeout.runtime_completed {
        return None;
    }
    if !closeout.missing_artifacts.is_empty() || !closeout.open_obligations.is_empty() {
        return Some(match closeout.closeout_class {
            ManualStCloseoutClass::EvidenceContradiction => "evidence_contradiction",
            _ => "open_obligation",
        });
    }
    if !closeout.verification_failed.is_empty() {
        return Some("verification_failed");
    }
    if !closeout.verification_required.is_empty() {
        return Some("verification_missing");
    }
    None
}

fn build_closeout_continuation_prompt(
    case_id: &str,
    stage_label: &str,
    attempt: usize,
    closeout: &ManualStCloseoutEvidence,
) -> String {
    if closeout_continuation_kind(closeout) == Some("verification_failed") {
        return build_verification_repair_continuation_prompt(
            case_id,
            stage_label,
            attempt,
            closeout,
        );
    }
    if closeout_continuation_kind(closeout) == Some("verification_missing") {
        return build_verification_missing_continuation_prompt(
            case_id,
            stage_label,
            attempt,
            closeout,
        );
    }
    let mut sections = vec![
        "Manual ST closeout continuation.".to_string(),
        "The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete. This is an explicit text-only user turn, equivalent to a Codex stop-hook continuation; it is not an assistant error retry.".to_string(),
        format!("Case: {case_id}"),
        format!("Stage: {stage_label}"),
        format!("Continuation attempt: {attempt}/{MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE}"),
        "Your next response must use file-changing tool calls such as write or apply_patch to create or update the missing artifacts before any final answer. A text-only promise about future work does not satisfy closeout.".to_string(),
    ];
    if let Some(message) = closeout
        .final_assistant_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
    {
        sections.push(format!("Previous final assistant message:\n{message}"));
    }
    sections.push(render_continuation_list(
        "Open obligations",
        &closeout.open_obligations,
    ));
    sections.push(render_continuation_list(
        "Missing expected artifacts",
        &closeout.missing_artifacts,
    ));
    sections.push(render_continuation_list(
        "Expected artifacts",
        &closeout.expected_artifacts,
    ));
    sections.push(render_continuation_list(
        "Required verification still missing",
        &closeout.verification_required,
    ));
    sections.push(render_continuation_list(
        "Required verification failed in the latest evidence",
        &closeout.verification_failed,
    ));
    sections.push(
        "When all artifacts are authored, run the required verification commands with shell and then provide a concise final answer.".to_string(),
    );
    sections.join("\n\n")
}

fn build_verification_repair_continuation_prompt(
    case_id: &str,
    stage_label: &str,
    attempt: usize,
    closeout: &ManualStCloseoutEvidence,
) -> String {
    let mut sections = vec![
        "Manual ST verification-repair continuation.".to_string(),
        "The prior assistant message completed a runtime turn, and all required artifacts are present, but the latest required verification command failed. This is an explicit text-only user turn, equivalent to a Codex stop-hook continuation; it is not an assistant error retry.".to_string(),
        format!("Case: {case_id}"),
        format!("Stage: {stage_label}"),
        format!("Verification-repair attempt: {attempt}/{MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE}"),
        "Your next response must make a content-changing repair with write or apply_patch before any final answer. Do not answer with a text-only promise. Do not rerun verification before editing the failing implementation.".to_string(),
    ];
    if let Some(message) = closeout
        .final_assistant_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
    {
        sections.push(format!("Previous final assistant message:\n{message}"));
    }
    sections.push(render_continuation_list(
        "Repair targets",
        &closeout.repair_targets,
    ));
    sections.push(render_continuation_list(
        "Failed required verification commands",
        &closeout.verification_failed,
    ));
    sections.push(render_continuation_list(
        "Latest verification failure evidence",
        &closeout.verification_failure_evidence,
    ));
    sections.push(render_continuation_list(
        "Expected artifacts",
        &closeout.expected_artifacts,
    ));
    sections.push(
        "After the repair edit, rerun the failed required verification command(s) with shell. Only provide a final answer after the rerun passes.".to_string(),
    );
    sections.join("\n\n")
}

fn build_verification_missing_continuation_prompt(
    case_id: &str,
    stage_label: &str,
    attempt: usize,
    closeout: &ManualStCloseoutEvidence,
) -> String {
    let mut sections = vec![
        "Manual ST verification continuation.".to_string(),
        "The prior assistant message completed a runtime turn, but required verification evidence is still missing. This is an explicit text-only user turn, equivalent to a Codex stop-hook continuation; it is not an assistant error retry.".to_string(),
        format!("Case: {case_id}"),
        format!("Stage: {stage_label}"),
        format!("Verification attempt: {attempt}/{MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE}"),
        "Your next response must run the missing required verification command(s) with shell before any final answer.".to_string(),
    ];
    sections.push(render_continuation_list(
        "Required verification still missing",
        &closeout.verification_required,
    ));
    sections.push(render_continuation_list(
        "Expected artifacts",
        &closeout.expected_artifacts,
    ));
    sections.push(
        "If verification fails, repair the failing implementation with write or apply_patch, then rerun the failed command.".to_string(),
    );
    sections.join("\n\n")
}

fn image_paths_for_closeout_attempt(
    case_id: &str,
    closeout_continuations: usize,
) -> Vec<Utf8PathBuf> {
    if closeout_continuations == 0 {
        image_paths_for_case(case_id)
    } else {
        Vec::new()
    }
}

fn render_continuation_list(label: &str, values: &[String]) -> String {
    if values.is_empty() {
        return format!("{label}:\n- none");
    }
    let items = values
        .iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{label}:\n{items}")
}

fn open_obligations_from_active_work(
    active_work: Option<&ActiveWorkContract>,
    verification_passed: &[String],
) -> (Vec<String>, Vec<String>) {
    let Some(active_work) = active_work else {
        return (Vec::new(), Vec::new());
    };
    let mut open_obligations = Vec::new();
    let mut verification_required = Vec::new();
    match active_work {
        ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets,
            verification_commands,
        } => {
            open_obligations.extend(
                pending_targets
                    .iter()
                    .filter(|target| closeout_target_is_deliverable_artifact(target.as_str()))
                    .map(|target| format!("author `{}`", target.as_str())),
            );
            verification_required.extend(
                verification_commands
                    .iter()
                    .filter(|command| {
                        !verification_command_was_passed(command, verification_passed)
                    })
                    .cloned(),
            );
        }
        ActiveWorkContract::DocsRepair {
            deliverable,
            pending_deliverables,
            ..
        } => {
            if let Some(deliverable) = deliverable {
                open_obligations.push(format!("repair docs `{}`", deliverable.as_str()));
            }
            open_obligations.extend(
                pending_deliverables
                    .iter()
                    .map(|item| format!("repair docs deliverable `{}`", item.target.as_str())),
            );
        }
        ActiveWorkContract::Verification {
            commands,
            repair_required,
            targets,
            ..
        } => {
            if *repair_required {
                open_obligations.extend(
                    targets
                        .iter()
                        .map(|target| format!("repair `{}`", target.as_str())),
                );
            }
            verification_required.extend(
                commands
                    .iter()
                    .filter(|command| {
                        !verification_command_was_passed(command, verification_passed)
                    })
                    .cloned(),
            );
        }
    }
    (
        dedupe_strings(open_obligations),
        dedupe_strings(verification_required),
    )
}

fn repair_targets_from_active_work(
    active_work: Option<&ActiveWorkContract>,
    expected_artifacts: &[String],
) -> Vec<String> {
    let mut targets = Vec::new();
    if let Some(ActiveWorkContract::Verification {
        repair_required: true,
        targets: active_targets,
        ..
    }) = active_work
    {
        targets.extend(
            active_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .filter(|target| !target.trim().is_empty()),
        );
    }
    if targets.is_empty() {
        targets.extend(
            expected_artifacts
                .iter()
                .filter(|artifact| likely_repair_source_artifact(artifact))
                .cloned(),
        );
    }
    dedupe_strings(targets)
}

fn closeout_target_is_deliverable_artifact(target: &str) -> bool {
    let normalized = target.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    let extension = filename.rsplit('.').next().unwrap_or_default();
    if filename.is_empty() {
        return false;
    }
    if !normalized.contains('/') && normalized.matches('.').count() > 1 {
        return false;
    }
    if matches!(
        filename,
        "readme" | "readme.md" | "changelog" | "changelog.md"
    ) {
        return true;
    }
    matches!(
        extension,
        "md" | "rst"
            | "adoc"
            | "txt"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "kt"
            | "go"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "scala"
            | "sh"
            | "ps1"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
    ) || (normalized.contains('/') && filename.starts_with("test_"))
}

fn likely_repair_source_artifact(artifact: &str) -> bool {
    let lower = artifact.to_ascii_lowercase();
    lower.ends_with(".py")
        && !lower.starts_with("test_")
        && !lower.contains("/test_")
        && !lower.ends_with("_test.py")
}

fn render_verification_failure_evidence(evidence: &VerificationCommandEvidence) -> String {
    let mut sections = vec![format!("command: {}", evidence.command)];
    if !evidence.stdout_summary.trim().is_empty() {
        sections.push(format!(
            "stdout: {}",
            truncate_continuation_evidence(&evidence.stdout_summary)
        ));
    }
    if !evidence.stderr_summary.trim().is_empty() {
        sections.push(format!(
            "stderr: {}",
            truncate_continuation_evidence(&evidence.stderr_summary)
        ));
    }
    if let Some(class) = evidence.normalized_failure_class.as_deref() {
        sections.push(format!("failure_class: {class}"));
    }
    sections.join("\n")
}

fn truncate_continuation_evidence(value: &str) -> String {
    let mut text = value.trim().to_string();
    if text.len() > 2_000 {
        text.truncate(2_000);
        text.push_str("\n[truncated]");
    }
    text
}

fn verification_command_was_passed(command: &str, passed: &[String]) -> bool {
    let normalized = normalize_command_for_route_evidence(command);
    passed
        .iter()
        .any(|value| normalize_command_for_route_evidence(value) == normalized)
}

fn normalize_command_for_route_evidence(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.contains(&value) {
            deduped.push(value);
        }
    }
    deduped
}

fn latest_final_assistant_text(history_items: &[HistoryItem]) -> Option<String> {
    let start = history_items
        .iter()
        .rposition(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. } | HistoryItemPayload::ToolOutput { .. }
            )
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let text = history_items[start..]
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::Message {
                role: MessageRole::Assistant,
                content,
                ..
            } => Some(
                content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.as_str()),
                        ContentPart::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn final_message_promises_future_work(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("next ")
        || lowered.contains("i will")
        || lowered.contains("i'll")
        || lowered.contains("will create")
        || lowered.contains("will update")
        || text.contains("次に")
        || text.contains("これから")
        || text.contains("作成します")
        || text.contains("更新します")
        || text.contains("実施します")
        || text.contains("対応します")
}

fn final_message_claims_completion(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("completed")
        || lowered.contains("done")
        || lowered.contains("created")
        || lowered.contains("updated")
        || text.contains("完了")
        || text.contains("作成しました")
        || text.contains("更新しました")
        || text.contains("確認しました")
}

fn open_case_store(data_dir: &Utf8Path) -> Result<SqliteStore, String> {
    let paths = StoragePaths {
        data_dir: data_dir.to_path_buf(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let store = SqliteStore::open(&paths).map_err(|error| error.to_string())?;
    store.migrate().map_err(|error| error.to_string())?;
    Ok(store)
}

fn validate_preflight_report(path: &Utf8Path) -> Result<(), String> {
    let bytes = fs::read(path.as_std_path())
        .map_err(|error| format!("failed to read preflight report `{path}`: {error}"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("preflight report `{path}` is not valid JSON: {error}"))?;
    if value.get("status").and_then(Value::as_str) != Some("pass") {
        return Err(format!(
            "preflight report `{path}` is not pass; representative route will not start"
        ));
    }
    Ok(())
}

fn prepare_case_workspace(
    case_id: &str,
    spec: &ManualStCaseSpec,
    previous_workspace: Option<&Utf8PathBuf>,
    workspace: &Utf8Path,
) -> Result<(), String> {
    if case_id == "case3" {
        let Some(previous) = previous_workspace else {
            return Err("case3 requires a passed case1 workspace baseline".to_string());
        };
        copy_workspace_contents(previous, workspace)?;
    }
    if case_id == "case5" {
        copy_external_fixture_if_available(
            &root_project_sandbox().join("RippleFish"),
            workspace,
            "case5 RippleFish fixture",
        )?;
    }
    if case_id == "case7" {
        copy_external_fixture_if_available(
            &root_project_sandbox().join("Sample_docs"),
            workspace,
            "case7 Sample_docs fixture",
        )?;
    }
    if let Some(task_file) = &spec.task_file {
        fs::write(workspace.join("task.md").as_std_path(), task_file)
            .map_err(|error| format!("failed to write task.md for {case_id}: {error}"))?;
    }
    if case_id.starts_with("case2") {
        let root = manual_st_root().join("case2");
        for name in ["scenario_contract.md", "scenario_contract.json"] {
            let source = root.join(name);
            if source.exists() {
                fs::copy(source.as_std_path(), workspace.join(name).as_std_path())
                    .map_err(|error| format!("failed to copy {name}: {error}"))?;
            }
        }
    }
    Ok(())
}

fn copy_external_fixture_if_available(
    source: &Utf8Path,
    workspace: &Utf8Path,
    label: &str,
) -> Result<(), String> {
    if !source.exists() {
        return Err(format!(
            "{label} source `{source}` does not exist; route cannot start without its canonical fixture"
        ));
    }
    copy_workspace_contents(source, workspace)
}

fn copy_workspace_contents(source: &Utf8Path, target: &Utf8Path) -> Result<(), String> {
    for entry in fs::read_dir(source.as_std_path())
        .map_err(|error| format!("failed to read `{source}`: {error}"))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|_| "workspace path is not valid UTF-8".to_string())?;
        let name = path
            .file_name()
            .ok_or_else(|| format!("path `{path}` has no file name"))?;
        if name == ".git" || name == ".moyai" {
            continue;
        }
        let destination = target.join(name);
        if path.is_dir() {
            fs::create_dir_all(destination.as_std_path()).map_err(|error| error.to_string())?;
            copy_workspace_contents(&path, &destination)?;
        } else if path.is_file() {
            fs::copy(path.as_std_path(), destination.as_std_path())
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ManualStCaseSpec {
    case_id: String,
    stages: Vec<ManualStStage>,
    expected_artifacts: Vec<String>,
    task_file: Option<String>,
}

#[derive(Debug, Clone)]
struct ManualStStage {
    label: String,
    prompt: String,
    verification_commands: Vec<String>,
}

impl ManualStCaseSpec {
    fn load(case_id: &str) -> Result<Self, String> {
        let base_case_id = case_id
            .strip_suffix('a')
            .or_else(|| case_id.strip_suffix('b'))
            .or_else(|| case_id.strip_suffix('c'))
            .unwrap_or(case_id);
        let path = manual_st_root().join(base_case_id).join("spec.md");
        let spec = fs::read_to_string(path.as_std_path())
            .map_err(|error| format!("failed to read manual ST spec `{path}`: {error}"))?;
        let prompts = extract_heading_fenced_blocks(&spec, "canonical user request");
        if prompts.is_empty() {
            return Err(format!("manual ST {case_id} has no canonical user request"));
        }
        let default_verification = extract_bulleted_backticks_after_heading(&spec, "verification");
        let stages = prompts
            .into_iter()
            .enumerate()
            .map(|(index, (heading, prompt))| {
                let verification_commands = if case_id == "case3" {
                    if heading.contains("stage1") || heading.contains("stage3") {
                        vec!["python -m unittest".to_string()]
                    } else {
                        Vec::new()
                    }
                } else {
                    default_verification.clone()
                };
                ManualStStage {
                    label: stage_label(&heading, index),
                    prompt,
                    verification_commands,
                }
            })
            .collect::<Vec<_>>();
        let mut expected_artifacts = extract_expected_artifacts(&spec);
        if case_id == "case7" && expected_artifacts.is_empty() {
            expected_artifacts.push("docs.md".to_string());
        }
        let task_file = extract_heading_fenced_blocks(&spec, "canonical task file")
            .into_iter()
            .next()
            .map(|(_, body)| body);
        Ok(Self {
            case_id: case_id.to_string(),
            stages,
            expected_artifacts,
            task_file,
        })
    }
}

fn stage_label(heading: &str, index: usize) -> String {
    heading
        .split_whitespace()
        .find(|part| part.starts_with("stage"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("stage{}", index + 1))
}

fn manual_st_root() -> Utf8PathBuf {
    option_env!("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| Utf8PathBuf::from("."))
        .join("tests")
        .join("manual_ST")
}

fn root_project_sandbox() -> Utf8PathBuf {
    option_env!("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .and_then(|path| path.parent().map(Utf8Path::to_path_buf))
        .unwrap_or_else(|| Utf8PathBuf::from("."))
        .join("project_sandbox")
}

fn default_manual_st_route_root(route: ManualStRouteKind, now_ms: i64) -> Utf8PathBuf {
    root_project_sandbox().join(format!("manual-st-route-{now_ms}-{}", route.route_id()))
}

pub fn manual_st_default_output_root_uses_workspace_sandbox_fixture_passes() -> bool {
    let default_root = default_manual_st_route_root(ManualStRouteKind::RequiredCore, 12345);
    let workspace_sandbox = root_project_sandbox();
    let manifest_sandbox = option_env!("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| Utf8PathBuf::from("."))
        .join("project_sandbox");

    default_root.starts_with(&workspace_sandbox) && !default_root.starts_with(&manifest_sandbox)
}

fn image_paths_for_case(case_id: &str) -> Vec<Utf8PathBuf> {
    if matches!(case_id, "case2" | "case2a" | "case2c") {
        let image = root_project_sandbox()
            .join("space_invader_png")
            .join("js-space_invaders01.jpg");
        if image.exists() {
            return vec![image];
        }
    }
    Vec::new()
}

fn extract_heading_fenced_blocks(markdown: &str, heading_contains: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut current_heading = None::<String>;
    let mut in_fence = false;
    let mut capture = false;
    let mut body = String::new();
    for line in markdown.lines() {
        if line.starts_with("## ") {
            current_heading = Some(line.trim_start_matches('#').trim().to_ascii_lowercase());
        }
        if line.trim_start().starts_with("```") {
            if !in_fence {
                in_fence = true;
                capture = current_heading
                    .as_deref()
                    .is_some_and(|heading| heading.contains(heading_contains));
                body.clear();
            } else {
                if capture {
                    blocks.push((
                        current_heading.clone().unwrap_or_default(),
                        body.trim_end().to_string(),
                    ));
                }
                in_fence = false;
                capture = false;
                body.clear();
            }
            continue;
        }
        if in_fence && capture {
            body.push_str(line);
            body.push('\n');
        }
    }
    blocks
}

fn extract_expected_artifacts(markdown: &str) -> Vec<String> {
    let mut artifacts = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            let heading = line.to_ascii_lowercase();
            in_section = heading.contains("必須成果物")
                || heading.contains("canonical expected artifact set");
            continue;
        }
        if in_section {
            artifacts.extend(extract_backticks(line));
        }
    }
    artifacts.sort();
    artifacts.dedup();
    artifacts
}

fn extract_bulleted_backticks_after_heading(markdown: &str, heading_contains: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            in_section = line.to_ascii_lowercase().contains(heading_contains);
            continue;
        }
        if in_section && line.trim_start().starts_with('-') {
            values.extend(extract_backticks(line));
        }
    }
    values
        .into_iter()
        .filter(|value| {
            value.contains("python ") || value.starts_with("cargo ") || value.starts_with("uv ")
        })
        .collect()
}

fn extract_backticks(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        values.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    values
}

async fn run_verification_command(
    command: &str,
    workspace: &Utf8Path,
    case_id: &str,
) -> Result<VerificationCommandEvidence, String> {
    let start_time = timestamp();
    let output = if cfg!(windows) {
        Command::new("powershell")
            .args(["-NoProfile", "-Command", command])
            .current_dir(workspace.as_std_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
    } else {
        Command::new("sh")
            .args(["-lc", command])
            .current_dir(workspace.as_std_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
    }
    .map_err(|error| format!("failed to run verification `{command}`: {error}"))?;
    Ok(VerificationCommandEvidence {
        command: command.to_string(),
        working_directory: workspace.to_string(),
        start_time,
        end_time: timestamp(),
        exit_code: output.status.code(),
        stdout_summary: summarize_bytes(&output.stdout),
        stderr_summary: summarize_bytes(&output.stderr),
        normalized_failure_class: (!output.status.success())
            .then(|| "verification_failed".to_string()),
        required: true,
        case_id: case_id.to_string(),
        requirement_id: None,
    })
}

fn summarize_bytes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut summary = text.lines().take(80).collect::<Vec<_>>().join("\n");
    if summary.len() > 8_000 {
        summary.truncate(8_000);
    }
    summary
}

fn write_route_artifacts(
    route_root: &Utf8Path,
    result: &ManualStRouteResult,
    verification_commands: Vec<VerificationCommandEvidence>,
    workspace_diff: WorkspaceDiff,
) -> Result<(), String> {
    write_json(route_root.join("result.json"), result)?;
    write_json(
        route_root.join("verification_command_log.json"),
        &json!({ "commands": verification_commands }),
    )?;
    write_json(
        route_root.join("workspace_diff_manifest.json"),
        &workspace_diff,
    )?;
    write_json(
        route_root.join("route_manifest.json"),
        &route_manifest(result),
    )?;
    if !route_root.join("timeout_classification.json").exists() {
        write_timeout_classification_with_reason(
            route_root,
            false,
            !matches!(result.route_level_verdict, RouteVerdict::Running),
            result.stop_reason.as_deref(),
        )?;
    }
    Ok(())
}

fn route_manifest(result: &ManualStRouteResult) -> Value {
    json!({
        "route_id": result.route_id,
        "case_ids": result.case_ids,
        "route_type": result.route_type,
        "build_identifier": result.build_identifier,
        "model_id": result.model_id,
        "provider_base_url": result.provider_base_url,
        "provider_metadata_summary": result.provider_metadata_summary,
        "provider_metadata_hash": result.provider_metadata_hash,
        "scenario_contract_hash": Value::Null,
        "fixture_version": FIXTURE_VERSION,
        "workspace_path": result.route_root,
        "session_id": result.session_ids.last().map(ToString::to_string),
        "start_time": result.started_at,
        "end_time": result.completed_at,
        "route_level_verdict": result.route_level_verdict,
        "evidence_artifacts": [
            "route_manifest.json",
            "verification_command_log.json",
            "workspace_diff_manifest.json",
            "result.json",
            "preflight_report.json",
            "timeout_classification.json"
        ]
    })
}

fn write_json(path: Utf8PathBuf, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    }
    fs::write(
        path.as_std_path(),
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("failed to write `{path}`: {error}"))
}

fn write_stage_events(
    case_root: &Utf8Path,
    stage_index: usize,
    events: &[RunEvent],
) -> Result<(), String> {
    let path = case_root.join(format!("stage{stage_index}.jsonl"));
    let mut file = fs::File::create(path.as_std_path()).map_err(|error| error.to_string())?;
    for event in events {
        writeln!(
            file,
            "{}",
            serde_json::to_string(event).map_err(|error| error.to_string())?
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn latest_session_failed_message(events: &[RunEvent]) -> Option<&str> {
    events.iter().rev().find_map(|event| match event {
        RunEvent::SessionFailed { message, .. } => Some(message.as_str()),
        _ => None,
    })
}

fn write_timeout_classification(
    root: &Utf8Path,
    outer_timeout: bool,
    classified_terminal_before_timeout: bool,
) -> Result<(), String> {
    write_timeout_classification_with_reason(
        root,
        outer_timeout,
        classified_terminal_before_timeout,
        None,
    )
}

fn write_timeout_classification_with_reason(
    root: &Utf8Path,
    outer_timeout: bool,
    classified_terminal_before_timeout: bool,
    reason: Option<&str>,
) -> Result<(), String> {
    write_json(
        root.join("timeout_classification.json"),
        &timeout_classification_value(outer_timeout, classified_terminal_before_timeout, reason),
    )
}

fn timeout_classification_value(
    outer_timeout: bool,
    classified_terminal_before_timeout: bool,
    reason: Option<&str>,
) -> Value {
    let provider_stream_stall = reason.is_some_and(is_provider_stream_stall_reason);
    let provider_transport_stream_error =
        reason.is_some_and(is_provider_transport_stream_error_reason);
    let semantic_no_progress_terminal =
        reason.is_some_and(is_semantic_no_progress_terminal_guard_reason);
    let evidence_refs = reason
        .filter(|_| {
            provider_stream_stall
                || provider_transport_stream_error
                || semantic_no_progress_terminal
        })
        .map(|reason| vec![reason.to_string()])
        .unwrap_or_default();
    let primary_timeout_owner = if outer_timeout {
        Some("harness_wait_policy")
    } else if provider_transport_stream_error {
        Some("provider_transport_stream_error")
    } else if provider_stream_stall {
        Some("provider_stream_idle_timeout")
    } else if semantic_no_progress_terminal {
        Some("semantic_no_progress_terminal_guard")
    } else {
        None
    };
    json!({
        "provider_stream_stall": provider_stream_stall,
        "provider_transport_stream_error": provider_transport_stream_error,
        "verification_non_convergence": false,
        "repeated_no_progress_repair": semantic_no_progress_terminal,
        "semantic_no_progress_terminal_guard": semantic_no_progress_terminal,
        "tool_or_environment_stall": false,
        "outer_timeout": outer_timeout,
        "classified_terminal_before_timeout": classified_terminal_before_timeout,
        "primary_timeout_owner": primary_timeout_owner,
        "evidence_refs": evidence_refs
    })
}

fn is_provider_stream_stall_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("provider stream idle timeout")
        || lower.contains("without any sse event")
        || lower.contains("stream disconnected before completion")
}

fn is_provider_transport_stream_error_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    (lower.contains("sse stream error") || lower.contains("response body"))
        && (lower.contains("transport error") || lower.contains("error decoding"))
}

fn is_semantic_no_progress_terminal_guard_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    (lower.contains("supporting-context budget")
        || lower.contains("supporting_context output")
        || lower.contains("representative survey budget is exhausted")
        || lower.contains("runtime stopped before allowing more broad docs-route discovery"))
        && (lower.contains("no-progress") || lower.contains("docs authoring"))
}

pub(crate) fn provider_stream_idle_timeout_classification_fixture_passes() -> bool {
    let reason = "case2a stage1 failed before completion: provider stream idle timeout after 300000ms without any SSE event";
    let value = timeout_classification_value(false, true, Some(reason));
    value.get("provider_stream_stall").and_then(Value::as_bool) == Some(true)
        && value.get("primary_timeout_owner").and_then(Value::as_str)
            == Some("provider_stream_idle_timeout")
        && value
            .get("evidence_refs")
            .and_then(Value::as_array)
            .is_some_and(|refs| refs.iter().any(|item| item.as_str() == Some(reason)))
}

pub(crate) fn provider_transport_stream_error_classification_fixture_passes() -> bool {
    let reason = "case2c stage1 failed before completion: run agent error: agent llm error: SSE stream error: Transport error: error decoding response body";
    let value = timeout_classification_value(false, true, Some(reason));
    value
        .get("provider_transport_stream_error")
        .and_then(Value::as_bool)
        == Some(true)
        && value.get("primary_timeout_owner").and_then(Value::as_str)
            == Some("provider_transport_stream_error")
        && value
            .get("evidence_refs")
            .and_then(Value::as_array)
            .is_some_and(|refs| refs.iter().any(|item| item.as_str() == Some(reason)))
}

pub(crate) fn semantic_no_progress_terminal_classification_fixture_passes() -> bool {
    let reason = "case5 stage1 ended with session status Failed: Docs route supporting-context budget was exhausted and the model repeated budget-exhausted discovery 3 time(s) instead of producing file-change evidence. Runtime stopped before growing provider history with more no-progress tool calls. Open docs targets: README.md, basic_design.md, detail_design.md.";
    let value = timeout_classification_value(false, true, Some(reason));
    value
        .get("semantic_no_progress_terminal_guard")
        .and_then(Value::as_bool)
        == Some(true)
        && value
            .get("repeated_no_progress_repair")
            .and_then(Value::as_bool)
            == Some(true)
        && value.get("primary_timeout_owner").and_then(Value::as_str)
            == Some("semantic_no_progress_terminal_guard")
        && value
            .get("evidence_refs")
            .and_then(Value::as_array)
            .is_some_and(|refs| refs.iter().any(|item| item.as_str() == Some(reason)))
}

pub(crate) fn route_evidence_filters_generated_dependency_paths_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    let _ = fs::create_dir_all(root.join("frontend/node_modules/pkg").as_std_path());
    let _ = fs::create_dir_all(root.join("backend/__pycache__").as_std_path());
    let _ = fs::create_dir_all(root.join("backend/data/memory/run-1").as_std_path());
    let _ = fs::create_dir_all(root.join("backend/data/runs/run-1").as_std_path());
    let _ = fs::create_dir_all(root.join("data/runs/run-1").as_std_path());
    let _ = fs::create_dir_all(root.join("backend/data/reports").as_std_path());
    let _ = fs::create_dir_all(root.join("backend/app").as_std_path());
    let _ = fs::write(
        root.join("frontend/node_modules/pkg/index.js")
            .as_std_path(),
        "generated",
    );
    let _ = fs::write(
        root.join("backend/__pycache__/mod.pyc").as_std_path(),
        "generated",
    );
    let _ = fs::write(
        root.join("backend/data/memory/run-1/agent-01.json")
            .as_std_path(),
        "{}",
    );
    let _ = fs::write(
        root.join("backend/data/runs/run-1/events.jsonl")
            .as_std_path(),
        "{}",
    );
    let _ = fs::write(root.join("data/runs/run-1/log.txt").as_std_path(), "{}");
    let _ = fs::write(
        root.join("backend/data/reports/generated.md").as_std_path(),
        "generated",
    );
    let _ = fs::write(root.join("backend/app/main.py").as_std_path(), "source");
    let Ok(files) = list_workspace_files(root.as_path()) else {
        return false;
    };
    files.contains(&"backend/app/main.py".to_string())
        && !files.iter().any(|file| {
            file.contains("node_modules")
                || file.contains("__pycache__")
                || file.contains("data/memory")
                || file.contains("data/runs")
                || file.contains("data/reports")
        })
}

pub(crate) fn route_evidence_overwrites_stale_timeout_classification_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(route_root) = Utf8PathBuf::from_path_buf(temp.path().join("route")) else {
        return false;
    };
    let case_root = route_root.join("case5");
    if fs::create_dir_all(case_root.as_std_path()).is_err() {
        return false;
    }
    if write_timeout_classification_with_reason(&route_root, false, true, None).is_err() {
        return false;
    }
    let reason = "case5 stage1 failed before completion: provider stream idle timeout after 300000ms without any SSE event";
    if write_timeout_classification_with_reason(&case_root, false, true, Some(reason)).is_err() {
        return false;
    }
    if propagate_case_timeout_classification(&route_root, &case_root, RouteVerdict::Fail).is_err() {
        return false;
    }
    let Ok(value) =
        fs::read_to_string(route_root.join("timeout_classification.json").as_std_path())
    else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&value) else {
        return false;
    };
    if json.get("primary_timeout_owner").and_then(Value::as_str)
        != Some("provider_stream_idle_timeout")
    {
        return false;
    }
    if reset_route_output_root(&route_root, ManualStRouteKind::ExtendedCase5).is_err() {
        return false;
    }
    !route_root.join("timeout_classification.json").exists() && !case_root.exists()
}

fn list_workspace_files(workspace: &Utf8Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_files(workspace, workspace, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(
    root: &Utf8Path,
    current: &Utf8Path,
    files: &mut Vec<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(current.as_std_path())
        .map_err(|error| format!("failed to read `{current}`: {error}"))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|_| "workspace file path is not valid UTF-8".to_string())?;
        let name = path.file_name().unwrap_or_default();
        if name == ".git" || name == ".moyai" {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_path_buf();
        if crate::agent::state::docs_route_path_is_generated_or_dependency(relative.as_path()) {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            files.push(relative.as_str().replace('\\', "/"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualStRouteResult {
    pub route_id: String,
    pub route_type: String,
    pub route_root: Utf8PathBuf,
    pub case_ids: Vec<String>,
    pub model_id: String,
    pub provider_base_url: String,
    pub provider_metadata_summary: Value,
    pub provider_metadata_hash: Option<String>,
    pub build_identifier: String,
    pub expected_artifacts: Vec<String>,
    pub route_level_verdict: RouteVerdict,
    pub session_ids: Vec<SessionId>,
    pub case_results: Vec<ManualStCaseResult>,
    pub started_at: String,
    pub completed_at: String,
    pub stop_reason: Option<String>,
}

impl ManualStRouteResult {
    fn started(config: &ManualStRouteRunConfig, route_root: Utf8PathBuf) -> Self {
        let case_ids = config
            .route
            .case_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let expected_artifacts = case_ids
            .iter()
            .filter_map(|case_id| ManualStCaseSpec::load(case_id).ok())
            .flat_map(|spec| spec.expected_artifacts)
            .collect::<Vec<_>>();
        let now = timestamp();
        Self {
            route_id: config.route.route_id().to_string(),
            route_type: config.route.route_type().to_string(),
            route_root,
            case_ids,
            model_id: config
                .model_override
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL_ID.to_string()),
            provider_base_url: config
                .base_url_override
                .clone()
                .unwrap_or_else(|| DEFAULT_PROVIDER_BASE_URL.to_string()),
            provider_metadata_summary: json!({
                "source": "configured_model_gate",
                "preflight_report": config.preflight_report
            }),
            provider_metadata_hash: None,
            build_identifier: build_identifier(),
            expected_artifacts,
            route_level_verdict: RouteVerdict::NotRun,
            session_ids: Vec::new(),
            case_results: Vec::new(),
            started_at: now.clone(),
            completed_at: now,
            stop_reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualStCaseResult {
    pub case_id: String,
    pub verdict: RouteVerdict,
    pub session_ids: Vec<SessionId>,
    pub expected_artifacts: Vec<String>,
    pub actual_files: Vec<String>,
    pub verification_commands: Vec<VerificationCommandEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closeout_evidence: Option<ManualStCloseoutEvidence>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub timeout_observed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualStCloseoutClass {
    CleanCloseout,
    RuntimeDidNotComplete,
    IncompleteOpenObligation,
    ContinuationPromised,
    EvidenceContradiction,
    VerificationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualStCloseoutEvidence {
    pub runtime_completed: bool,
    pub closeout_class: ManualStCloseoutClass,
    pub final_assistant_message: Option<String>,
    pub open_obligations: Vec<String>,
    pub expected_artifacts: Vec<String>,
    pub missing_artifacts: Vec<String>,
    pub verification_required: Vec<String>,
    pub verification_passed: Vec<String>,
    pub verification_failed: Vec<String>,
    #[serde(default)]
    pub verification_failure_evidence: Vec<String>,
    #[serde(default)]
    pub repair_targets: Vec<String>,
    pub diagnostics: Vec<String>,
}

pub fn final_assistant_open_obligation_not_clean_closeout_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some(
            "calculator.py was created. Next I will create test_calculator.py and run tests."
                .to_string(),
        ),
        Some(&active_work),
        &[
            "calculator.py".to_string(),
            "test_calculator.py".to_string(),
        ],
        &["calculator.py".to_string()],
        &[],
    );

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && evidence.runtime_completed
        && evidence
            .missing_artifacts
            .contains(&"test_calculator.py".to_string())
        && evidence
            .open_obligations
            .contains(&"author `test_calculator.py`".to_string())
        && !evidence.diagnostics.is_empty()
}

pub fn final_assistant_open_obligation_continuation_hook_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will create test_calculator.py.".to_string()),
        Some(&active_work),
        &[
            "calculator.py".to_string(),
            "test_calculator.py".to_string(),
        ],
        &["calculator.py".to_string()],
        &[],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && should_continue_after_closeout(&evidence)
        && prompt.contains("explicit text-only user turn")
        && prompt.contains("text-only user turn")
        && prompt.contains("Codex stop-hook continuation")
        && prompt.contains("test_calculator.py")
        && prompt.contains("python -m unittest")
        && prompt.contains("must use file-changing tool calls")
        && prompt.contains("text-only promise about future work does not satisfy closeout")
        && !prompt.contains("[error]")
        && !prompt.contains("required_next_action")
        && !prompt.contains("tool_choice=required")
}

pub fn closeout_continuation_is_text_only_fixture_passes() -> bool {
    image_paths_for_closeout_attempt("case2a", 0).len() == 1
        && image_paths_for_closeout_attempt("case2a", 1).is_empty()
        && image_paths_for_closeout_attempt("case2a", MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE)
            .is_empty()
}

pub fn vision_prompt_uses_labeled_attachment_fixture_passes() -> bool {
    let spec_path = manual_st_root().join("case2").join("spec.md");
    let Ok(spec) = std::fs::read_to_string(spec_path.as_std_path()) else {
        return false;
    };
    spec.contains("添付画像 [Image #1]")
        && spec.contains("provider-visible image item")
        && spec.contains("再発見する必要はありません")
        && !spec.contains("添付画像 `js-space_invaders01.jpg`")
}

pub fn latest_verification_result_drives_closeout_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: vec![Utf8PathBuf::from("test_calculator.py")],
    };
    let failed = VerificationCommandEvidence {
        command: "python   -m   unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "NO TESTS RAN".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let passed = VerificationCommandEvidence {
        command: "python -m unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "2".to_string(),
        end_time: "3".to_string(),
        exit_code: Some(0),
        stdout_summary: "Ran 3 tests".to_string(),
        stderr_summary: String::new(),
        normalized_failure_class: None,
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("All work is complete.".to_string()),
        Some(&active_work),
        &[
            "calculator.py".to_string(),
            "test_calculator.py".to_string(),
        ],
        &[
            "calculator.py".to_string(),
            "test_calculator.py".to_string(),
        ],
        &[failed, passed],
    );

    evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && evidence
            .verification_passed
            .contains(&"python -m unittest".to_string())
        && evidence.verification_failed.is_empty()
        && evidence.verification_required.is_empty()
}

pub fn verification_failure_preserves_closeout_evidence_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let verification = VerificationCommandEvidence {
        command: "python -m unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "NO TESTS RAN".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "case1".to_string(),
        requirement_id: None,
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some(
            "calculator.py was created. Next I will create test_calculator.py and run tests."
                .to_string(),
        ),
        Some(&active_work),
        &[
            "calculator.py".to_string(),
            "test_calculator.py".to_string(),
        ],
        &["calculator.py".to_string()],
        &[verification],
    );

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && evidence
            .verification_failed
            .contains(&"python -m unittest".to_string())
        && evidence
            .missing_artifacts
            .contains(&"test_calculator.py".to_string())
        && evidence
            .open_obligations
            .contains(&"author `test_calculator.py`".to_string())
        && evidence
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("required verification failed"))
}

pub fn verification_failed_closeout_builds_repair_hook_prompt_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["public_api".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("space_invader.py")],
    };
    let verification = VerificationCommandEvidence {
        command: "python -m unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "AttributeError: 'GameState' object has no attribute 'update_bullets'"
            .to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public-api".to_string()),
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("The tests failed; next I will fix space_invader.py.".to_string()),
        Some(&active_work),
        &[
            "README.md".to_string(),
            "scenario_contract.json".to_string(),
            "scenario_contract.md".to_string(),
            "space_invader.py".to_string(),
            "test_space_invader.py".to_string(),
        ],
        &[
            "README.md".to_string(),
            "scenario_contract.json".to_string(),
            "scenario_contract.md".to_string(),
            "space_invader.py".to_string(),
            "test_space_invader.py".to_string(),
        ],
        &[verification],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && should_continue_after_closeout(&evidence)
        && evidence
            .repair_targets
            .contains(&"space_invader.py".to_string())
        && evidence
            .verification_failed
            .contains(&"python -m unittest".to_string())
        && prompt.contains("verification-repair continuation")
        && prompt.contains("explicit text-only user turn")
        && prompt.contains("Codex stop-hook continuation")
        && prompt.contains("space_invader.py")
        && prompt.contains("python -m unittest")
        && prompt.contains("update_bullets")
        && prompt.contains("write or apply_patch")
        && prompt.contains("rerun the failed required verification command")
        && prompt.contains("Do not answer with a text-only promise")
        && !prompt.contains("[error]")
        && !prompt.contains("required_next_action")
        && !prompt.contains("tool_choice=required")
}

pub fn closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("test_space_invader.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let open_evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will create test_space_invader.py.".to_string()),
        Some(&active_work),
        &[
            "space_invader.py".to_string(),
            "test_space_invader.py".to_string(),
        ],
        &["space_invader.py".to_string()],
        &[],
    );
    let repair_active_work = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["public_api".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("space_invader.py")],
    };
    let failed_verification = VerificationCommandEvidence {
        command: "python -m unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "AssertionError: public behavior mismatch".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let repair_evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will fix space_invader.py.".to_string()),
        Some(&repair_active_work),
        &[
            "space_invader.py".to_string(),
            "test_space_invader.py".to_string(),
        ],
        &[
            "space_invader.py".to_string(),
            "test_space_invader.py".to_string(),
        ],
        &[failed_verification],
    );

    let mut budget = CloseoutContinuationBudget::default();
    let mut exhausted_open = Vec::new();
    for _ in 0..MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE {
        exhausted_open.push(budget.next_attempt(&open_evidence));
    }
    exhausted_open.iter().all(|attempt| attempt.is_some())
        && budget.next_attempt(&open_evidence).is_none()
        && budget.next_attempt(&repair_evidence) == Some(1)
        && closeout_continuation_signature(&open_evidence)
            != closeout_continuation_signature(&repair_evidence)
}

pub fn verification_failure_labels_do_not_become_authoring_obligations_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("test_space_invader.TestBulletClass.test_bullet_creation"),
            Utf8PathBuf::from("test_space_invader.TestBulletClass.test_bullet_destroy"),
            Utf8PathBuf::from("test_space_invader.TestBulletClass.test_bullet_rect"),
        ],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let verification = VerificationCommandEvidence {
        command: "python -m unittest".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary:
            "TypeError: Bullet.__init__() got an unexpected keyword argument 'is_enemy'".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public-api".to_string()),
    };
    let expected = vec![
        "README.md".to_string(),
        "scenario_contract.json".to_string(),
        "scenario_contract.md".to_string(),
        "space_invader.py".to_string(),
        "test_space_invader.py".to_string(),
    ];
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("現在のファイルの内容を確認します。".to_string()),
        Some(&active_work),
        &expected,
        &expected,
        &[verification],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && evidence.open_obligations.is_empty()
        && evidence.missing_artifacts.is_empty()
        && evidence
            .verification_failed
            .contains(&"python -m unittest".to_string())
        && evidence
            .repair_targets
            .contains(&"space_invader.py".to_string())
        && closeout_continuation_kind(&evidence) == Some("verification_failed")
        && prompt.contains("Manual ST verification-repair continuation")
        && prompt.contains("space_invader.py")
        && prompt.contains("python -m unittest")
        && prompt.contains("write or apply_patch")
        && !prompt.contains("author `test_space_invader.TestBulletClass")
        && !prompt.contains("tool_choice=required")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteVerdict {
    Running,
    Pass,
    Fail,
    Blocked,
    NotRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceDiff {
    expected_artifacts: Vec<String>,
    actual_added_files: Vec<String>,
    actual_modified_files: Vec<String>,
    actual_deleted_files: Vec<String>,
    unexpected_outside_workspace_access_or_change: bool,
    fixture_input_mutation: bool,
    verdict: String,
    diagnostics: Vec<String>,
}

impl WorkspaceDiff {
    fn empty() -> Self {
        Self {
            expected_artifacts: Vec::new(),
            actual_added_files: Vec::new(),
            actual_modified_files: Vec::new(),
            actual_deleted_files: Vec::new(),
            unexpected_outside_workspace_access_or_change: false,
            fixture_input_mutation: false,
            verdict: "blocked".to_string(),
            diagnostics: vec!["workspace was not executed".to_string()],
        }
    }

    fn from_workspace(workspace: &Utf8Path, expected_artifacts: &[String]) -> Result<Self, String> {
        let files = list_workspace_files(workspace)?;
        let missing = expected_artifacts
            .iter()
            .filter(|artifact| !files.contains(artifact))
            .cloned()
            .collect::<Vec<_>>();
        Ok(Self {
            expected_artifacts: expected_artifacts.to_vec(),
            actual_added_files: files,
            actual_modified_files: Vec::new(),
            actual_deleted_files: missing.clone(),
            unexpected_outside_workspace_access_or_change: false,
            fixture_input_mutation: false,
            verdict: if missing.is_empty() { "clean" } else { "dirty" }.to_string(),
            diagnostics: if missing.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "missing expected artifacts: {}",
                    missing.join(", ")
                )]
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationCommandEvidence {
    pub command: String,
    pub working_directory: String,
    pub start_time: String,
    pub end_time: String,
    pub exit_code: Option<i32>,
    pub stdout_summary: String,
    pub stderr_summary: String,
    pub normalized_failure_class: Option<String>,
    pub required: bool,
    pub case_id: String,
    pub requirement_id: Option<String>,
}

fn build_identifier() -> String {
    option_env!("CARGO_PKG_VERSION")
        .map(|version| format!("moyai-{version}"))
        .unwrap_or_else(|| "moyai-local".to_string())
}

fn timestamp() -> String {
    SystemClock::now_ms().to_string()
}

#[derive(Default)]
struct RecordingRenderer {
    events: Vec<RunEvent>,
}

impl EventRenderer for RecordingRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        self.events.push(event.clone());
        Ok(())
    }

    fn finish(&mut self, _summary: &RunSummary) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_list(
        &mut self,
        _sessions: &[crate::session::SessionRecord],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_show(
        &mut self,
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        _session: &crate::session::SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

struct HarnessConfirmationPrompt;

impl ConfirmationPrompt for HarnessConfirmationPrompt {
    fn confirm(&mut self, _request: &PermissionRequest) -> Result<bool, CliPromptError> {
        Ok(true)
    }
}
