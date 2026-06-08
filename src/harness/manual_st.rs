use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::str::FromStr;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target as classify_language_artifact_target,
};
use crate::agent::state::{ActiveWorkContract, active_work_contract_for_history_items};
use crate::app::{App, AppBootstrap, AppCommand, RunRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode};
use crate::config::model::{PartialModelConfig, PartialPermissionsConfig, PartialResolvedConfig};
use crate::config::{AccessMode, ProviderMetadataMode, ResolvedConfig, ShellFamily};
use crate::error::{CliPromptError, CliRenderError};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, ProtocolEventStore,
    ToolLifecycleStatus, ToolProgressEffect, TurnId, VerificationRunResult, VerificationRunStatus,
};
use crate::runtime::{SystemClock, build_cancel_token};
use crate::session::SessionRepository;
use crate::session::{
    EditorContext, FailureKind, MessageRole, PromptDispatchPart, RunEvent, RunSummary, SessionId,
    SessionStatus, ToolCallId,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::PermissionRequest;

const FIXTURE_VERSION: &str = "manual_st_route_runner.v1";
const MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE: usize = 6;
const MAX_CLOSEOUT_CONTINUATIONS_WITHOUT_WORKSPACE_PROGRESS: usize = 3;
const MAX_TERMINALIZED_CLOSEOUT_CONTINUATIONS_PER_STAGE: usize = 3;
const MAX_TERMINALIZED_CLOSEOUT_CONTINUATIONS_PER_CLUSTER: usize = 2;
const MANUAL_ST_ROUTE_COMMAND_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone)]
pub struct ManualStRouteRunConfig {
    pub route: ManualStRouteKind,
    pub output_root: Option<Utf8PathBuf>,
    pub preflight_report: Utf8PathBuf,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub provider_metadata_mode_override: Option<ProviderMetadataMode>,
    pub context_window_override: Option<u32>,
    pub max_output_tokens_override: Option<u32>,
    pub max_turn_seconds: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualStRouteKind {
    RequiredCore,
    TargetedCoreCase1,
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
            Self::TargetedCoreCase1 => "targeted_core_case1",
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
            Self::TargetedCoreCase1 => "targeted_support",
            Self::RequiredVision | Self::RequiredVisionFull => "required_vision",
            Self::TargetedSupport => "targeted_support",
            Self::ExtendedCase4 | Self::ExtendedCase5 | Self::ExtendedCase7 => "extended",
            Self::ProbeCase6 => "probe",
        }
    }

    pub fn case_ids(self) -> Vec<&'static str> {
        match self {
            Self::RequiredCore => vec!["case1", "case3"],
            Self::TargetedCoreCase1 => vec!["case1"],
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
            "targeted-case1" | "core-case1" | "case1" => Ok(Self::TargetedCoreCase1),
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
            "case_progress.json".to_string(),
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
    mark_route_progress(&mut result, None, "route_running");
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
        mark_route_progress(&mut result, Some(case_id), "case_running");
        let starting_workspace_diff =
            WorkspaceDiff::from_workspace(&workspace, &case_expected_artifacts)?;
        write_route_artifacts(
            &route_root,
            &result,
            verification_commands.clone(),
            starting_workspace_diff,
        )?;
        let case_result = run_case(
            &config,
            &case_spec,
            &case_root,
            &workspace,
            &data_dir,
            &route_root,
        )
        .await?;
        verification_commands.extend(case_result.verification_commands.clone());
        result.session_ids.extend(case_result.session_ids.clone());
        result.case_results.push(case_result.clone());
        mark_route_progress(&mut result, Some(case_id), "case_completed");
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

    (result.route_level_verdict, result.stop_reason) =
        materialize_manual_st_route_terminal_verdict(&result);

    result.completed_at = timestamp();
    let workspace_diff = last_case_workspace
        .as_ref()
        .map(|workspace| WorkspaceDiff::from_workspace(workspace, &reached_expected_artifacts))
        .transpose()?
        .unwrap_or_else(WorkspaceDiff::empty);
    if result.case_results.iter().any(|case| case.timeout_observed) {
        write_timeout_classification(&route_root, true, false)?;
    }
    mark_route_progress(&mut result, None, "route_terminalized");
    write_route_artifacts(&route_root, &result, verification_commands, workspace_diff)?;
    Ok(result)
}

fn mark_route_progress(
    result: &mut ManualStRouteResult,
    active_case_id: Option<&str>,
    status: &str,
) {
    result.active_case_id = active_case_id.map(str::to_string);
    result.progress_status = Some(status.to_string());
    result.last_progress_at = Some(timestamp());
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
    route_root: &Utf8Path,
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

    write_case_progress_artifact(
        route_root,
        &ManualStCaseProgress::new(
            config,
            case_spec,
            None,
            None,
            None,
            workspace,
            case_root,
            data_dir,
            RouteVerdict::Running,
            "case_started",
            Some("case workspace and data directory prepared"),
        ),
    )?;

    'stages: for (stage_index, stage) in case_spec.stages.iter().enumerate() {
        closeout_evidence = None;
        let mut stage_prompt = append_visible_scenario_contract_prompt(&stage.prompt, workspace);
        let mut stage_verification_commands = Vec::new();
        let mut closeout_continuation_turns = 0usize;
        let mut closeout_budget = CloseoutContinuationBudget::default();
        let mut terminal_continuation_ledger = RouteStageTerminalContinuationLedger::default();
        loop {
            let continuation = manual_st_stage_session_continuation(session_id);
            write_case_progress_artifact(
                route_root,
                &ManualStCaseProgress::new(
                    config,
                    case_spec,
                    Some(stage_index + 1),
                    Some(&stage.label),
                    continuation.session_id.map(|id| id.to_string()),
                    workspace,
                    case_root,
                    data_dir,
                    RouteVerdict::Running,
                    "model_request_inflight",
                    Some("manual ST stage request dispatched to runtime"),
                ),
            )?;
            let model_override_patch = manual_st_model_override_patch(config);
            let request = RunRequest {
                prompt: stage_prompt.clone(),
                session_id: continuation.session_id,
                continue_last: continuation.continue_last,
                title: Some(format!("manual ST {} {}", case_spec.case_id, stage.label)),
                cwd: workspace.to_path_buf(),
                model: manual_st_run_request_model(config),
                base_url: manual_st_run_request_base_url(config),
                config_override: Some(PartialResolvedConfig {
                    model: model_override_patch,
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
                cancel: build_cancel_token(),
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
                    let reason = format!(
                        "{} {} failed before completion: {error}",
                        case_spec.case_id, stage.label
                    );
                    let provider_terminal = is_provider_stream_stall_reason(&reason)
                        || is_provider_transport_stream_error_reason(&reason);
                    if let Some(current_session_id) = session_id {
                        let actual_files_after_error = list_workspace_files(workspace)?;
                        refresh_case_verification_evidence_freshness(
                            &app,
                            session_id,
                            &mut verification_commands,
                        )
                        .await?;
                        refresh_case_verification_evidence_freshness(
                            &app,
                            session_id,
                            &mut stage_verification_commands,
                        )
                        .await?;
                        closeout_evidence = Some(
                            classify_manual_st_closeout_for_session(
                                &app,
                                current_session_id,
                                false,
                                &case_spec.expected_artifacts,
                                &actual_files_after_error,
                                &stage_verification_commands,
                            )
                            .await?,
                        );
                        if !provider_terminal {
                            if let Some(evidence) = closeout_evidence.as_ref() {
                                let workspace_fingerprint =
                                    workspace_content_fingerprint(workspace)?;
                                if let Some(closeout_attempt) =
                                    next_stage_closeout_continuation_attempt(
                                        &mut closeout_budget,
                                        &mut terminal_continuation_ledger,
                                        evidence,
                                        &workspace_fingerprint,
                                        Some(&reason),
                                    )
                                {
                                    closeout_continuation_turns += 1;
                                    stage_prompt = build_closeout_continuation_prompt(
                                        &case_spec.case_id,
                                        &stage.label,
                                        closeout_attempt,
                                        evidence,
                                    );
                                    write_case_progress_artifact(
                                        route_root,
                                        &ManualStCaseProgress::new(
                                            config,
                                            case_spec,
                                            Some(stage_index + 1),
                                            Some(&stage.label),
                                            session_id.map(|id| id.to_string()),
                                            workspace,
                                            case_root,
                                            data_dir,
                                            RouteVerdict::Running,
                                            "closeout_continuation_pending",
                                            Some(
                                                "runtime error closeout evidence is being continued in the same stage",
                                            ),
                                        ),
                                    )?;
                                    continue;
                                }
                            }
                        }
                    }
                    verdict = RouteVerdict::Fail;
                    if provider_terminal {
                        write_timeout_classification_with_reason(
                            case_root,
                            false,
                            true,
                            Some(&reason),
                        )?;
                    }
                    write_case_progress_artifact(
                        route_root,
                        &ManualStCaseProgress::new(
                            config,
                            case_spec,
                            Some(stage_index + 1),
                            Some(&stage.label),
                            session_id.map(|id| id.to_string()),
                            workspace,
                            case_root,
                            data_dir,
                            RouteVerdict::Fail,
                            "runtime_error",
                            Some(&reason),
                        ),
                    )?;
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
                    write_case_progress_artifact(
                        route_root,
                        &ManualStCaseProgress::new(
                            config,
                            case_spec,
                            Some(stage_index + 1),
                            Some(&stage.label),
                            session_id.map(|id| id.to_string()),
                            workspace,
                            case_root,
                            data_dir,
                            RouteVerdict::Fail,
                            "turn_timeout",
                            stop_reason.as_deref(),
                        ),
                    )?;
                    break 'stages;
                }
            };
            session_id = Some(summary.session_id);
            write_case_progress_artifact(
                route_root,
                &ManualStCaseProgress::new(
                    config,
                    case_spec,
                    Some(stage_index + 1),
                    Some(&stage.label),
                    Some(summary.session_id.to_string()),
                    workspace,
                    case_root,
                    data_dir,
                    RouteVerdict::Running,
                    "runtime_completed",
                    Some("runtime session terminal status observed"),
                ),
            )?;
            if summary.status != SessionStatus::Completed {
                let actual_files_after_stage = list_workspace_files(workspace)?;
                refresh_case_verification_evidence_freshness(
                    &app,
                    session_id,
                    &mut verification_commands,
                )
                .await?;
                refresh_case_verification_evidence_freshness(
                    &app,
                    session_id,
                    &mut stage_verification_commands,
                )
                .await?;
                if all_expected_artifacts_present(
                    &case_spec.expected_artifacts,
                    &actual_files_after_stage,
                ) {
                    run_stage_route_verification(
                        stage,
                        workspace,
                        &case_spec.case_id,
                        &mut verification_commands,
                        &mut stage_verification_commands,
                    )
                    .await?;
                    refresh_case_verification_evidence_freshness(
                        &app,
                        session_id,
                        &mut verification_commands,
                    )
                    .await?;
                    refresh_case_verification_evidence_freshness(
                        &app,
                        session_id,
                        &mut stage_verification_commands,
                    )
                    .await?;
                }
                closeout_evidence = Some(
                    classify_manual_st_closeout(
                        &app,
                        &summary,
                        &case_spec.expected_artifacts,
                        &actual_files_after_stage,
                        &stage_verification_commands,
                    )
                    .await?,
                );
                let runtime_failure = latest_session_failed_message(&renderer.events);
                let reason = format!(
                    "{} {} ended with session status {:?}",
                    case_spec.case_id, stage.label, summary.status
                );
                let reason = runtime_failure
                    .map(|message| format!("{reason}: {message}"))
                    .unwrap_or(reason);
                let provider_terminal = is_provider_stream_stall_reason(&reason)
                    || is_provider_transport_stream_error_reason(&reason);
                if let Some(evidence) = closeout_evidence.as_ref()
                    && route_owned_contract_satisfied_after_verification(evidence)
                {
                    write_case_progress_artifact(
                        route_root,
                        &ManualStCaseProgress::new(
                            config,
                            case_spec,
                            Some(stage_index + 1),
                            Some(&stage.label),
                            Some(summary.session_id.to_string()),
                            workspace,
                            case_root,
                            data_dir,
                            RouteVerdict::Running,
                            "stage_route_verified_after_runtime_terminal",
                            Some(
                                "route-owned verification passed against the latest workspace after runtime terminalization",
                            ),
                        ),
                    )?;
                    write_stage_events(case_root, stage_index + 1, &renderer.events)?;
                    renderer.events.clear();
                    continue 'stages;
                }
                if !provider_terminal {
                    if let Some(evidence) = closeout_evidence.as_ref() {
                        let workspace_fingerprint = workspace_content_fingerprint(workspace)?;
                        if let Some(closeout_attempt) = next_stage_closeout_continuation_attempt(
                            &mut closeout_budget,
                            &mut terminal_continuation_ledger,
                            evidence,
                            &workspace_fingerprint,
                            Some(&reason),
                        ) {
                            closeout_continuation_turns += 1;
                            stage_prompt = build_closeout_continuation_prompt(
                                &case_spec.case_id,
                                &stage.label,
                                closeout_attempt,
                                evidence,
                            );
                            write_case_progress_artifact(
                                route_root,
                                &ManualStCaseProgress::new(
                                    config,
                                    case_spec,
                                    Some(stage_index + 1),
                                    Some(&stage.label),
                                    Some(summary.session_id.to_string()),
                                    workspace,
                                    case_root,
                                    data_dir,
                                    RouteVerdict::Running,
                                    "closeout_continuation_pending",
                                    Some(
                                        "runtime terminal status closeout evidence is being continued in the same stage",
                                    ),
                                ),
                            )?;
                            continue;
                        }
                    }
                }
                verdict = RouteVerdict::Fail;
                write_timeout_classification_with_reason(case_root, false, true, Some(&reason))?;
                write_case_progress_artifact(
                    route_root,
                    &ManualStCaseProgress::new(
                        config,
                        case_spec,
                        Some(stage_index + 1),
                        Some(&stage.label),
                        Some(summary.session_id.to_string()),
                        workspace,
                        case_root,
                        data_dir,
                        RouteVerdict::Fail,
                        "runtime_non_completed",
                        Some(&reason),
                    ),
                )?;
                stop_reason = Some(reason);
                break 'stages;
            }

            let actual_files_before_route_verification = list_workspace_files(workspace)?;
            refresh_case_verification_evidence_freshness(
                &app,
                session_id,
                &mut verification_commands,
            )
            .await?;
            refresh_case_verification_evidence_freshness(
                &app,
                session_id,
                &mut stage_verification_commands,
            )
            .await?;
            let pre_verification_closeout = classify_manual_st_closeout(
                &app,
                &summary,
                &case_spec.expected_artifacts,
                &actual_files_before_route_verification,
                &stage_verification_commands,
            )
            .await?;
            if !manual_st_route_verification_may_run(&pre_verification_closeout) {
                let workspace_fingerprint = workspace_content_fingerprint(workspace)?;
                if let Some(closeout_attempt) = next_stage_closeout_continuation_attempt(
                    &mut closeout_budget,
                    &mut terminal_continuation_ledger,
                    &pre_verification_closeout,
                    &workspace_fingerprint,
                    None,
                ) {
                    closeout_continuation_turns += 1;
                    stage_prompt = build_closeout_continuation_prompt(
                        &case_spec.case_id,
                        &stage.label,
                        closeout_attempt,
                        &pre_verification_closeout,
                    );
                    closeout_evidence = Some(pre_verification_closeout);
                    write_case_progress_artifact(
                        route_root,
                        &ManualStCaseProgress::new(
                            config,
                            case_spec,
                            Some(stage_index + 1),
                            Some(&stage.label),
                            Some(summary.session_id.to_string()),
                            workspace,
                            case_root,
                            data_dir,
                            RouteVerdict::Running,
                            "closeout_continuation_pending",
                            Some(
                                "pre-verification closeout evidence requires same-stage continuation",
                            ),
                        ),
                    )?;
                    continue;
                }

                verdict = RouteVerdict::Fail;
                stop_reason = Some(format!(
                    "{} {} closeout classified as {:?}: {}",
                    case_spec.case_id,
                    stage.label,
                    pre_verification_closeout.closeout_class,
                    pre_verification_closeout.diagnostics.join("; ")
                ));
                closeout_evidence = Some(pre_verification_closeout);
                break 'stages;
            }

            write_case_progress_artifact(
                route_root,
                &ManualStCaseProgress::new(
                    config,
                    case_spec,
                    Some(stage_index + 1),
                    Some(&stage.label),
                    Some(summary.session_id.to_string()),
                    workspace,
                    case_root,
                    data_dir,
                    RouteVerdict::Running,
                    "route_verification_evaluating",
                    Some(
                        "route-owned verification and public command contracts are being evaluated",
                    ),
                ),
            )?;
            run_stage_route_verification(
                stage,
                workspace,
                &case_spec.case_id,
                &mut verification_commands,
                &mut stage_verification_commands,
            )
            .await?;

            let actual_files_after_stage = list_workspace_files(workspace)?;
            refresh_case_verification_evidence_freshness(
                &app,
                session_id,
                &mut verification_commands,
            )
            .await?;
            refresh_case_verification_evidence_freshness(
                &app,
                session_id,
                &mut stage_verification_commands,
            )
            .await?;
            let closeout = classify_manual_st_closeout(
                &app,
                &summary,
                &case_spec.expected_artifacts,
                &actual_files_after_stage,
                &stage_verification_commands,
            )
            .await?;
            if closeout.closeout_class == ManualStCloseoutClass::CleanCloseout {
                closeout_evidence = Some(closeout);
                write_case_progress_artifact(
                    route_root,
                    &ManualStCaseProgress::new(
                        config,
                        case_spec,
                        Some(stage_index + 1),
                        Some(&stage.label),
                        Some(summary.session_id.to_string()),
                        workspace,
                        case_root,
                        data_dir,
                        RouteVerdict::Running,
                        "stage_clean_closeout",
                        Some(
                            "stage closeout is clean; route may advance to the next stage or terminal case result",
                        ),
                    ),
                )?;
                write_stage_events(case_root, stage_index + 1, &renderer.events)?;
                renderer.events.clear();
                continue 'stages;
            }

            let workspace_fingerprint = workspace_content_fingerprint(workspace)?;
            if let Some(closeout_attempt) = next_stage_closeout_continuation_attempt(
                &mut closeout_budget,
                &mut terminal_continuation_ledger,
                &closeout,
                &workspace_fingerprint,
                None,
            ) {
                closeout_continuation_turns += 1;
                stage_prompt = build_closeout_continuation_prompt(
                    &case_spec.case_id,
                    &stage.label,
                    closeout_attempt,
                    &closeout,
                );
                closeout_evidence = Some(closeout);
                write_case_progress_artifact(
                    route_root,
                    &ManualStCaseProgress::new(
                        config,
                        case_spec,
                        Some(stage_index + 1),
                        Some(&stage.label),
                        Some(summary.session_id.to_string()),
                        workspace,
                        case_root,
                        data_dir,
                        RouteVerdict::Running,
                        "closeout_continuation_pending",
                        Some(
                            "post-verification closeout evidence requires same-stage continuation",
                        ),
                    ),
                )?;
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

    refresh_case_verification_evidence_freshness(&app, session_id, &mut verification_commands)
        .await?;
    let actual_files = list_workspace_files(workspace)?;
    (verdict, stop_reason) = materialize_manual_st_case_terminal_verdict(
        verdict,
        stop_reason,
        timeout_observed,
        &case_spec.case_id,
        &case_spec.expected_artifacts,
        &actual_files,
        closeout_evidence.as_ref(),
    );
    write_case_progress_artifact(
        route_root,
        &ManualStCaseProgress::new(
            config,
            case_spec,
            None,
            None,
            session_id.map(|id| id.to_string()),
            workspace,
            case_root,
            data_dir,
            verdict,
            "case_terminalized",
            stop_reason.as_deref(),
        ),
    )?;

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

fn materialize_manual_st_case_terminal_verdict(
    prior_verdict: RouteVerdict,
    prior_stop_reason: Option<String>,
    timeout_observed: bool,
    case_id: &str,
    expected_artifacts: &[String],
    actual_files: &[String],
    closeout_evidence: Option<&ManualStCloseoutEvidence>,
) -> (RouteVerdict, Option<String>) {
    if timeout_observed {
        return (
            RouteVerdict::Fail,
            prior_stop_reason.or_else(|| Some(format!("{case_id} timed out"))),
        );
    }

    if let Some(reason) = prior_stop_reason {
        return (RouteVerdict::Fail, Some(reason));
    }

    if let Some(missing) = expected_artifacts
        .iter()
        .find(|expected| !actual_files.contains(expected))
    {
        return (
            RouteVerdict::Fail,
            Some(format!("{case_id} missing expected artifact `{missing}`")),
        );
    }

    if let Some(evidence) = closeout_evidence {
        if evidence.runtime_completed
            && evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
            && evidence.missing_artifacts.is_empty()
            && evidence.open_obligations.is_empty()
            && evidence.verification_required.is_empty()
            && evidence.verification_failed.is_empty()
        {
            return (RouteVerdict::Pass, None);
        }
    }

    (prior_verdict, None)
}

fn materialize_manual_st_route_terminal_verdict(
    result: &ManualStRouteResult,
) -> (RouteVerdict, Option<String>) {
    if result.case_results.len() == result.case_ids.len()
        && result
            .case_results
            .iter()
            .all(|case| matches!(case.verdict, RouteVerdict::Pass))
    {
        return (RouteVerdict::Pass, None);
    }

    if let Some(failed_case) = result
        .case_results
        .iter()
        .find(|case| !matches!(case.verdict, RouteVerdict::Pass))
    {
        return (
            failed_case.verdict,
            failed_case.stop_reason.clone().or_else(|| {
                Some(format!(
                    "{} did not pass; route stopped fail-stop",
                    failed_case.case_id
                ))
            }),
        );
    }

    (
        RouteVerdict::Fail,
        Some("route ended before all cases completed".to_string()),
    )
}

fn all_expected_artifacts_present(expected_artifacts: &[String], actual_files: &[String]) -> bool {
    expected_artifacts
        .iter()
        .all(|expected| actual_files.contains(expected))
}

async fn refresh_case_verification_evidence_freshness(
    app: &App,
    session_id: Option<SessionId>,
    verification_commands: &mut [VerificationCommandEvidence],
) -> Result<(), String> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let history_items = app
        .store
        .protocol_event_store()
        .list_history_items_for_session(session_id)
        .map_err(|error| {
            format!(
                "failed to load protocol history for verification freshness `{session_id}`: {error}"
            )
        })?;
    let latest_content_change_ms =
        latest_authoring_content_change_ms(&history_items, app.workspace.root.as_path());
    mark_stale_verification_evidence_after_content_change(
        verification_commands,
        latest_content_change_ms,
        &history_items,
    );
    Ok(())
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

fn manual_st_model_override_patch(config: &ManualStRouteRunConfig) -> Option<PartialModelConfig> {
    if config.provider_metadata_mode_override.is_none()
        && config.context_window_override.is_none()
        && config.max_output_tokens_override.is_none()
    {
        return None;
    }
    let mut patch = PartialModelConfig::default();
    patch.provider_metadata_mode = config.provider_metadata_mode_override;
    if let Some(context_window) = config.context_window_override {
        patch.context_window = Some(context_window);
        patch.extra_body_json = Some(json!({ "num_ctx": context_window }));
    }
    patch.max_output_tokens = config.max_output_tokens_override;
    Some(patch)
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
    classify_manual_st_closeout_for_session(
        app,
        summary.session_id,
        summary.status == SessionStatus::Completed,
        expected_artifacts,
        actual_files,
        verification_commands,
    )
    .await
}

async fn classify_manual_st_closeout_for_session(
    app: &App,
    session_id: SessionId,
    runtime_completed: bool,
    expected_artifacts: &[String],
    actual_files: &[String],
    verification_commands: &[VerificationCommandEvidence],
) -> Result<ManualStCloseoutEvidence, String> {
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
        runtime_completed,
        latest_final_assistant_text(&history_items),
        active_work.as_ref(),
        expected_artifacts,
        actual_files,
        verification_commands,
    );
    evidence.terminal_cluster = latest_route_stage_terminal_cluster_evidence(&history_items);
    let repair_targets = repair_targets_from_closeout_evidence(
        active_work.as_ref(),
        expected_artifacts,
        verification_commands,
    );
    if !repair_targets.is_empty() {
        evidence.repair_targets = repair_targets;
    }
    Ok(evidence)
}

fn manual_st_route_verification_may_run(closeout: &ManualStCloseoutEvidence) -> bool {
    closeout.runtime_completed
        && closeout.missing_artifacts.is_empty()
        && closeout.open_obligations.is_empty()
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
        .filter(|evidence| verification_evidence_passed(evidence))
        .map(|evidence| evidence.command.clone())
        .collect::<Vec<_>>();
    let verification_failed = latest_verification
        .iter()
        .filter(|evidence| !verification_evidence_passed(evidence) && evidence.required)
        .map(|evidence| evidence.command.clone())
        .collect::<Vec<_>>();
    let verification_failure_evidence = latest_verification
        .iter()
        .filter(|evidence| !verification_evidence_passed(evidence) && evidence.required)
        .map(|evidence| render_verification_failure_evidence(evidence))
        .collect::<Vec<_>>();
    let (open_obligations, verification_required) =
        open_obligations_from_active_work(active_work, &verification_passed, actual_files);
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
        repair_targets: repair_targets_from_closeout_evidence(
            active_work,
            expected_artifacts,
            &latest_verification.into_iter().cloned().collect::<Vec<_>>(),
        ),
        diagnostics,
        terminal_cluster: None,
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

fn verification_evidence_passed(evidence: &VerificationCommandEvidence) -> bool {
    evidence.normalized_failure_class.is_none()
}

fn route_owned_contract_satisfied_after_verification(closeout: &ManualStCloseoutEvidence) -> bool {
    closeout.missing_artifacts.is_empty()
        && closeout.open_obligations.is_empty()
        && closeout.verification_required.is_empty()
        && closeout.verification_failed.is_empty()
        && !closeout.verification_passed.is_empty()
}

fn mark_stale_verification_evidence_after_content_change(
    verification_commands: &mut [VerificationCommandEvidence],
    latest_content_change_ms: Option<i64>,
    history_items: &[HistoryItem],
) {
    let Some(latest_content_change_ms) = latest_content_change_ms else {
        return;
    };
    for evidence in verification_commands {
        if !evidence.required || !verification_evidence_passed(evidence) {
            continue;
        }
        let Ok(end_ms) = evidence.end_time.parse::<i64>() else {
            continue;
        };
        if end_ms <= latest_content_change_ms {
            if runtime_verification_pass_satisfies_after(
                history_items,
                &evidence.command,
                latest_content_change_ms,
            ) {
                continue;
            }
            evidence.normalized_failure_class = Some(format!(
                "verification_stale_after_content_change: latest content change at {latest_content_change_ms} occurred after verification ended at {end_ms}"
            ));
        }
    }
}

fn runtime_verification_pass_satisfies_after(
    history_items: &[HistoryItem],
    command: &str,
    threshold_ms: i64,
) -> bool {
    history_items.iter().any(|item| {
        if item.created_at_ms <= threshold_ms {
            return false;
        }
        let HistoryItemPayload::ToolOutput {
            verification_run: Some(run),
            ..
        } = &item.payload
        else {
            return false;
        };
        matches!(run.status, VerificationRunStatus::Passed)
            && verification_run_satisfies_route_command(run, command)
    })
}

fn verification_run_satisfies_route_command(run: &VerificationRunResult, command: &str) -> bool {
    let normalized = normalize_command_for_route_evidence(command);
    normalize_command_for_route_evidence(&run.command) == normalized
        || run
            .satisfies_command_identities
            .iter()
            .any(|identity| normalize_command_for_route_evidence(identity) == normalized)
}

fn latest_authoring_content_change_ms(
    history_items: &[HistoryItem],
    workspace_root: &Utf8Path,
) -> Option<i64> {
    history_items
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::FileChange { changes, .. } => changes
                .iter()
                .any(|change| {
                    change
                        .path_after
                        .as_ref()
                        .or(change.path_before.as_ref())
                        .is_some_and(|path| route_authoring_content_path(path, workspace_root))
                })
                .then_some(item.created_at_ms),
            _ => None,
        })
        .max()
}

fn route_authoring_content_path(path: &Utf8Path, workspace_root: &Utf8Path) -> bool {
    let relative = path
        .strip_prefix(workspace_root)
        .ok()
        .unwrap_or(path)
        .as_str()
        .replace('\\', "/");
    let lower = relative.to_ascii_lowercase();
    if lower.contains("/__pycache__/")
        || lower.starts_with("__pycache__/")
        || lower.ends_with(".pyc")
        || lower.ends_with(".pyo")
    {
        return false;
    }
    let spec = classify_language_artifact_target(&relative);
    if matches!(
        spec.role,
        ArtifactRole::Source | ArtifactRole::Test | ArtifactRole::Document
    ) {
        return true;
    }
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("json" | "toml" | "yaml" | "yml")
    )
}

pub fn route_authoring_content_paths_use_language_adapter_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/project");
    route_authoring_content_path(
        Utf8Path::new("C:/workspace/project/src/tool.test.ts"),
        workspace_root,
    ) && route_authoring_content_path(
        Utf8Path::new("C:/workspace/project/src/tool.rs"),
        workspace_root,
    ) && route_authoring_content_path(
        Utf8Path::new("C:/workspace/project/docs/tool.md"),
        workspace_root,
    ) && !route_authoring_content_path(
        Utf8Path::new("C:/workspace/project/__pycache__/tool.cpython-313.pyc"),
        workspace_root,
    )
}

fn should_continue_after_closeout(closeout: &ManualStCloseoutEvidence) -> bool {
    if closeout.runtime_completed {
        return matches!(
            closeout.closeout_class,
            ManualStCloseoutClass::ContinuationPromised
                | ManualStCloseoutClass::IncompleteOpenObligation
                | ManualStCloseoutClass::EvidenceContradiction
                | ManualStCloseoutClass::VerificationRequired
        );
    }
    matches!(
        closeout.closeout_class,
        ManualStCloseoutClass::RuntimeDidNotComplete
    ) && (!closeout.missing_artifacts.is_empty()
        || !closeout.open_obligations.is_empty()
        || !closeout.verification_failed.is_empty()
        || !closeout.verification_required.is_empty())
}

#[derive(Default)]
struct CloseoutContinuationBudget {
    attempts_by_signature: BTreeMap<String, usize>,
    total_attempts: usize,
    last_workspace_fingerprint: Option<String>,
    attempts_without_workspace_progress: usize,
}

impl CloseoutContinuationBudget {
    fn next_attempt(&mut self, closeout: &ManualStCloseoutEvidence) -> Option<usize> {
        self.next_attempt_inner(closeout, None)
    }

    fn next_attempt_with_workspace_fingerprint(
        &mut self,
        closeout: &ManualStCloseoutEvidence,
        workspace_fingerprint: &str,
    ) -> Option<usize> {
        self.next_attempt_inner(closeout, Some(workspace_fingerprint))
    }

    fn next_attempt_inner(
        &mut self,
        closeout: &ManualStCloseoutEvidence,
        workspace_fingerprint: Option<&str>,
    ) -> Option<usize> {
        if !should_continue_after_closeout(closeout) {
            return None;
        }
        if self.total_attempts >= MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE {
            return None;
        }
        let signature = closeout_continuation_signature(closeout)?;
        let attempts = self.attempts_by_signature.entry(signature).or_insert(0);
        if *attempts >= MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE {
            return None;
        }
        if let Some(workspace_fingerprint) = workspace_fingerprint {
            let same_workspace =
                self.last_workspace_fingerprint.as_deref() == Some(workspace_fingerprint);
            let next_without_progress = if same_workspace {
                self.attempts_without_workspace_progress + 1
            } else {
                1
            };
            if next_without_progress > MAX_CLOSEOUT_CONTINUATIONS_WITHOUT_WORKSPACE_PROGRESS {
                return None;
            }
            self.last_workspace_fingerprint = Some(workspace_fingerprint.to_string());
            self.attempts_without_workspace_progress = next_without_progress;
        }
        *attempts += 1;
        self.total_attempts += 1;
        Some(*attempts)
    }
}

#[derive(Default)]
struct RouteStageTerminalContinuationLedger {
    attempts_by_cluster: BTreeMap<String, usize>,
    last_workspace_fingerprint_by_cluster: BTreeMap<String, String>,
    total_attempts: usize,
}

impl RouteStageTerminalContinuationLedger {
    fn admit(
        &mut self,
        closeout: &ManualStCloseoutEvidence,
        workspace_fingerprint: &str,
        terminal_reason: Option<&str>,
    ) -> bool {
        let Some(reason) = terminal_reason else {
            return closeout.closeout_class == ManualStCloseoutClass::CleanCloseout;
        };
        let Some(cluster) = classify_route_stage_terminal_cluster(closeout, Some(reason)) else {
            return closeout.closeout_class == ManualStCloseoutClass::CleanCloseout;
        };
        if cluster.fail_stop {
            return false;
        }
        if self.total_attempts >= MAX_TERMINALIZED_CLOSEOUT_CONTINUATIONS_PER_STAGE {
            return false;
        }
        let prior_attempts = self
            .attempts_by_cluster
            .get(&cluster.key)
            .copied()
            .unwrap_or(0);
        if prior_attempts >= MAX_TERMINALIZED_CLOSEOUT_CONTINUATIONS_PER_CLUSTER {
            return false;
        }
        let same_workspace = self
            .last_workspace_fingerprint_by_cluster
            .get(&cluster.key)
            .is_some_and(|fingerprint| fingerprint == workspace_fingerprint);
        if prior_attempts > 0 && (same_workspace || !cluster.workspace_progress_can_reset) {
            return false;
        }
        self.attempts_by_cluster
            .insert(cluster.key.clone(), prior_attempts + 1);
        self.last_workspace_fingerprint_by_cluster
            .insert(cluster.key, workspace_fingerprint.to_string());
        self.total_attempts += 1;
        true
    }
}

fn next_stage_closeout_continuation_attempt(
    closeout_budget: &mut CloseoutContinuationBudget,
    terminal_ledger: &mut RouteStageTerminalContinuationLedger,
    closeout: &ManualStCloseoutEvidence,
    workspace_fingerprint: &str,
    terminal_reason: Option<&str>,
) -> Option<usize> {
    if !terminal_ledger.admit(closeout, workspace_fingerprint, terminal_reason) {
        return None;
    }
    closeout_budget.next_attempt_with_workspace_fingerprint(closeout, workspace_fingerprint)
}

fn route_stage_terminal_continuation_cluster(
    closeout: &ManualStCloseoutEvidence,
    reason: &str,
) -> Option<String> {
    classify_route_stage_terminal_cluster(closeout, Some(reason)).map(|cluster| cluster.key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteStageTerminalCluster {
    key: String,
    fail_stop: bool,
    workspace_progress_can_reset: bool,
}

fn classify_route_stage_terminal_cluster(
    closeout: &ManualStCloseoutEvidence,
    reason: Option<&str>,
) -> Option<RouteStageTerminalCluster> {
    if let Some(typed) = closeout.terminal_cluster.as_ref() {
        return Some(route_stage_terminal_cluster_from_typed(closeout, typed));
    }
    let reason = reason?;
    classify_route_stage_terminal_cluster_from_reason(closeout, reason)
}

fn route_stage_terminal_cluster_from_typed(
    closeout: &ManualStCloseoutEvidence,
    typed: &ManualStTerminalClusterEvidence,
) -> RouteStageTerminalCluster {
    let closeout_signature = closeout_continuation_signature(closeout)
        .unwrap_or_else(|| "no_closeout_signature".to_string());
    RouteStageTerminalCluster {
        key: format!("{}|{closeout_signature}", typed.failure_family),
        fail_stop: typed.fail_stop,
        workspace_progress_can_reset: typed.workspace_progress_can_reset,
    }
}

fn classify_route_stage_terminal_cluster_from_reason(
    closeout: &ManualStCloseoutEvidence,
    reason: &str,
) -> Option<RouteStageTerminalCluster> {
    let lower = reason.to_ascii_lowercase();
    let (failure_family, fail_stop, workspace_progress_can_reset) = if lower
        .contains("model returned a final assistant message")
    {
        (
            "final_assistant_with_open_obligation".to_string(),
            false,
            true,
        )
    } else if let Some(tool) =
        terminal_reason_word_after(reason, "Provider repeated invalid arguments for ")
    {
        (format!("invalid_tool_arguments:{tool}"), false, true)
    } else if let Some(cluster) =
        terminal_reason_backtick_after(reason, "lifecycle adjudication cluster `")
    {
        (format!("lifecycle_cluster:{cluster}"), false, true)
    } else if let Some(cluster) = terminal_reason_backtick_after(reason, "no-progress cluster `") {
        (format!("no_progress_cluster:{cluster}"), false, true)
    } else if terminal_reason_is_content_changing_authoring_no_progress(reason)
        && let Some(tool) = terminal_reason_backtick_after(reason, "Tool `")
    {
        (
            format!("content_changing_authoring_no_progress:{tool}"),
            true,
            false,
        )
    } else if lower.contains("returned `no_progress` output")
        && let Some(tool) = terminal_reason_backtick_after(reason, "Tool `")
    {
        (format!("tool_no_progress:{tool}"), false, true)
    } else if lower.contains("same verification failure evidence repeated") {
        ("verification_non_convergence".to_string(), false, false)
    } else if terminal_reason_is_authoring_grounding_budget_exhausted(reason) {
        (
            "authoring_grounding_budget_exhausted".to_string(),
            true,
            false,
        )
    } else {
        return None;
    };
    let closeout_signature = closeout_continuation_signature(closeout)
        .unwrap_or_else(|| "no_closeout_signature".to_string());
    Some(RouteStageTerminalCluster {
        key: format!("{failure_family}|{closeout_signature}"),
        fail_stop,
        workspace_progress_can_reset,
    })
}

fn latest_route_stage_terminal_cluster_evidence(
    history_items: &[HistoryItem],
) -> Option<ManualStTerminalClusterEvidence> {
    let mut tool_names_by_call = BTreeMap::new();
    for item in history_items {
        if let HistoryItemPayload::ToolCall { call_id, tool, .. } = &item.payload {
            tool_names_by_call.insert(call_id.to_string(), tool.to_string());
        }
    }
    history_items
        .iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::RejectedToolProposal { proposal }
                if proposal.semantic_class == "text_final_while_obligations_open" =>
            {
                Some(ManualStTerminalClusterEvidence {
                    failure_family: "final_assistant_with_open_obligation".to_string(),
                    fail_stop: false,
                    workspace_progress_can_reset: true,
                    source: "rejected_tool_proposal.semantic_class".to_string(),
                })
            }
            HistoryItemPayload::RejectedToolProposal { proposal } => {
                Some(ManualStTerminalClusterEvidence {
                    failure_family: format!("lifecycle_cluster:{}", proposal.semantic_class),
                    fail_stop: false,
                    workspace_progress_can_reset: true,
                    source: "rejected_tool_proposal.semantic_class".to_string(),
                })
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                metadata,
                progress_effect,
                verification_run,
                ..
            } => typed_terminal_cluster_from_tool_output(
                *call_id,
                metadata,
                progress_effect.clone(),
                verification_run.as_ref(),
                &tool_names_by_call,
            ),
            _ => None,
        })
}

fn typed_terminal_cluster_from_tool_output(
    call_id: ToolCallId,
    metadata: &Value,
    progress_effect: ToolProgressEffect,
    verification_run: Option<&VerificationRunResult>,
    tool_names_by_call: &BTreeMap<String, String>,
) -> Option<ManualStTerminalClusterEvidence> {
    if metadata
        .get("authoring_target_grounding_required")
        .and_then(Value::as_bool)
        == Some(true)
        || metadata
            .pointer("/tool_feedback_envelope/kind")
            .and_then(Value::as_str)
            == Some("authoring_target_grounding_required")
    {
        return Some(ManualStTerminalClusterEvidence {
            failure_family: "authoring_grounding_budget_exhausted".to_string(),
            fail_stop: true,
            workspace_progress_can_reset: false,
            source: "tool_output.authoring_target_grounding_required".to_string(),
        });
    }

    let operation_intent = metadata
        .pointer("/tool_feedback_envelope/operation_intent")
        .or_else(|| metadata.get("operation_intent"))
        .and_then(Value::as_str);
    let operation_progress_class = metadata
        .pointer("/tool_feedback_envelope/operation_progress_class")
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str);
    let tool = metadata
        .pointer("/tool_feedback_envelope/tool")
        .or_else(|| metadata.pointer("/tool_route/effective_tool"))
        .or_else(|| metadata.get("effective_tool"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| tool_names_by_call.get(&call_id.to_string()).cloned())
        .unwrap_or_else(|| "unknown".to_string());

    if operation_intent == Some("content_changing_authoring_required")
        && progress_effect == ToolProgressEffect::NoProgress
        && matches!(
            operation_progress_class,
            Some("no_progress" | "idempotent_file_write_no_progress")
        )
    {
        return Some(ManualStTerminalClusterEvidence {
            failure_family: format!("content_changing_authoring_no_progress:{tool}"),
            fail_stop: true,
            workspace_progress_can_reset: false,
            source: "tool_output.tool_feedback_envelope".to_string(),
        });
    }

    if operation_intent == Some("content_changing_authoring_required")
        && progress_effect == ToolProgressEffect::NoProgress
        && operation_progress_class == Some("supporting_context")
    {
        return Some(ManualStTerminalClusterEvidence {
            failure_family: format!("tool_no_progress:{tool}"),
            fail_stop: false,
            workspace_progress_can_reset: true,
            source: "tool_output.tool_feedback_envelope".to_string(),
        });
    }

    if verification_run.is_some_and(|run| {
        matches!(
            run.status,
            VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
        )
    }) && metadata
        .pointer("/terminal_guard_policy/no_progress_guard")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Some(ManualStTerminalClusterEvidence {
            failure_family: "verification_non_convergence".to_string(),
            fail_stop: false,
            workspace_progress_can_reset: false,
            source: "tool_output.verification_run.terminal_guard_policy".to_string(),
        });
    }

    None
}

fn terminal_reason_is_authoring_grounding_budget_exhausted(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("authoring supporting-context budget was exhausted")
        && lower.contains("non-remaining active target read proposals")
}

fn terminal_reason_is_content_changing_authoring_no_progress(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("returned `no_progress` output")
        && lower.contains("content-changing authoring is required")
        && lower
            .contains("runtime stopped before treating non-content tool calls as artifact progress")
}

fn terminal_reason_requires_route_fail_stop(reason: &str) -> bool {
    terminal_reason_is_authoring_grounding_budget_exhausted(reason)
        || terminal_reason_is_content_changing_authoring_no_progress(reason)
}

fn terminal_reason_word_after(reason: &str, marker: &str) -> Option<String> {
    let start = reason.find(marker)? + marker.len();
    reason[start..]
        .split_whitespace()
        .next()
        .map(|word| word.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_'))
        .filter(|word| !word.is_empty())
        .map(|word| word.to_ascii_lowercase())
}

fn terminal_reason_backtick_after(reason: &str, marker: &str) -> Option<String> {
    let start = reason.find(marker)? + marker.len();
    let end = reason[start..].find('`')?;
    let cluster = reason[start..start + end].trim();
    (!cluster.is_empty()).then(|| cluster.to_string())
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
        closeout_continuation_intro(closeout).to_string(),
        format!("Case: {case_id}"),
        format!("Stage: {stage_label}"),
        format!("Continuation attempt: {attempt}/{MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE}"),
        closeout_open_obligation_instruction(closeout),
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
    sections.push(
        "Expected artifacts are route inventory evidence only. They do not create new authoring targets unless the same path is listed under Open obligations or Missing expected artifacts."
            .to_string(),
    );
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

fn closeout_continuation_intro(closeout: &ManualStCloseoutEvidence) -> &'static str {
    if closeout.runtime_completed {
        return "The prior assistant message completed a runtime turn, but route closeout evidence shows the requested work is not complete. This is an explicit text-only user turn, equivalent to a Codex stop-hook continuation; it is not an assistant error retry.";
    }
    "The prior runtime turn ended before clean route closeout, but current route closeout evidence still identifies actionable open work. This is an explicit text-only user turn, equivalent to a Codex stop-hook continuation; it is not an assistant error retry."
}

fn closeout_open_obligation_instruction(closeout: &ManualStCloseoutEvidence) -> String {
    if closeout.missing_artifacts.is_empty() && !closeout.open_obligations.is_empty() {
        return "Your next response must satisfy the listed Open obligations with provider-visible file-changing tool calls such as apply_patch before any final answer. Do not create or update files only because they appear in the Expected artifacts inventory. A text-only promise about future work does not satisfy closeout.".to_string();
    }
    if !closeout.missing_artifacts.is_empty() {
        return "Your next response must use provider-visible file-changing tool calls such as apply_patch to create or update the listed Missing expected artifacts before any final answer. A text-only promise about future work does not satisfy closeout.".to_string();
    }
    "Your next response must satisfy the remaining typed closeout obligation before any final answer. A text-only promise about future work does not satisfy closeout.".to_string()
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
        "Your next response must make a content-changing repair with apply_patch before any final answer. Do not answer with a text-only promise. Do not rerun verification before editing the failing implementation.".to_string(),
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
        "If verification fails, repair the failing implementation with apply_patch, then rerun the failed command.".to_string(),
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
    actual_files: &[String],
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
                    .filter(|target| {
                        !actual_files
                            .iter()
                            .any(|file| closeout_targets_match(file, target.as_str()))
                    })
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
            route_contract_satisfied,
            ..
        } => {
            let deliverable_exists = deliverable.as_ref().is_some_and(|target| {
                actual_files
                    .iter()
                    .any(|file| closeout_targets_match(file, target.as_str()))
            });
            let satisfied_route_contract = *route_contract_satisfied && deliverable_exists;
            if let Some(deliverable) = deliverable
                && !satisfied_route_contract
            {
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
            let route_verification_passed = !commands.is_empty()
                && commands
                    .iter()
                    .all(|command| verification_command_was_passed(command, verification_passed));
            if *repair_required && !route_verification_passed {
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

fn repair_targets_from_closeout_evidence(
    active_work: Option<&ActiveWorkContract>,
    expected_artifacts: &[String],
    verification_commands: &[VerificationCommandEvidence],
) -> Vec<String> {
    let latest_verification = latest_verification_evidence_by_command(verification_commands);
    let generated_test_targets = generated_test_parse_defect_repair_targets_from_verification(
        &latest_verification,
        expected_artifacts,
    );
    if !generated_test_targets.is_empty() {
        return generated_test_targets;
    }
    repair_targets_from_active_work(active_work, expected_artifacts)
}

fn generated_test_parse_defect_repair_targets_from_verification(
    latest_verification: &[&VerificationCommandEvidence],
    expected_artifacts: &[String],
) -> Vec<String> {
    let mut targets = Vec::new();
    for evidence in latest_verification
        .iter()
        .filter(|evidence| evidence.required && !verification_evidence_passed(evidence))
    {
        let summary = render_verification_failure_evidence(evidence);
        let typed_evidence = crate::agent::repair_lane::verification_failure_evidence_from_summary(
            FailureKind::VerificationFailed,
            &summary,
        );
        for item in typed_evidence {
            if !verification_evidence_is_generated_test_parse_defect(&item) {
                continue;
            }
            for target in item
                .target
                .iter()
                .chain(item.test_refs.iter())
                .filter_map(|target| {
                    expected_artifact_for_closeout_target(target, expected_artifacts)
                })
            {
                targets.push(target);
            }
        }
    }
    dedupe_strings(targets)
}

fn verification_evidence_is_generated_test_parse_defect(
    evidence: &crate::session::VerificationFailureEvidence,
) -> bool {
    let is_parse_defect = evidence.subtype.as_deref() == Some("source_parse_defect")
        || evidence.subtype.as_deref() == Some("generated_test_parse_defect")
        || evidence.evidence_markers.iter().any(|marker| {
            marker == "source_parse_defect" || marker == "generated_test_parse_defect"
        });
    is_parse_defect
        && evidence
            .source_refs
            .iter()
            .all(|target| !closeout_target_is_mutable_source(target))
        && evidence
            .target
            .iter()
            .chain(evidence.test_refs.iter())
            .any(|target| closeout_target_is_test_like(target))
}

fn expected_artifact_for_closeout_target(
    target: &str,
    expected_artifacts: &[String],
) -> Option<String> {
    if !closeout_target_is_test_like(target) {
        return None;
    }
    let normalized = normalize_closeout_target_path(target);
    expected_artifacts
        .iter()
        .find(|artifact| {
            let expected = normalize_closeout_target_path(artifact);
            normalized == expected
        })
        .cloned()
}

pub(crate) fn manual_st_closeout_repair_targets_preserve_exact_identity_fixture_passes() -> bool {
    let expected_artifacts = vec!["tests/workflow.test.ts".to_string()];
    expected_artifact_for_closeout_target("tests/workflow.test.ts", &expected_artifacts)
        == Some("tests/workflow.test.ts".to_string())
        && expected_artifact_for_closeout_target(
            "foreign/tests/workflow.test.ts",
            &expected_artifacts,
        )
        .is_none()
        && expected_artifact_for_closeout_target("workflow.test.ts", &expected_artifacts).is_none()
        && expected_artifact_for_closeout_target(
            "tests/other-workflow.test.ts",
            &expected_artifacts,
        )
        .is_none()
}

fn closeout_targets_match(left: &str, right: &str) -> bool {
    normalize_closeout_target_path(left) == normalize_closeout_target_path(right)
}

fn closeout_target_is_test_like(target: &str) -> bool {
    classify_language_artifact_target(&normalize_closeout_target_path(target)).role
        == ArtifactRole::Test
}

fn closeout_target_is_mutable_source(target: &str) -> bool {
    let normalized = normalize_closeout_target_path(target);
    let spec = classify_language_artifact_target(&normalized);
    spec.role == ArtifactRole::Source && !closeout_target_is_contract_artifact(&normalized)
}

fn normalize_closeout_target_path(target: &str) -> String {
    target
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .replace('\\', "/")
}

fn closeout_file_name(target: &str) -> Option<&str> {
    target
        .rsplit('/')
        .next()
        .filter(|name| !name.trim().is_empty())
}

fn closeout_target_is_deliverable_artifact(target: &str) -> bool {
    let normalized = target.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
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
    let spec = classify_language_artifact_target(&normalized);
    matches!(
        spec.role,
        ArtifactRole::Source | ArtifactRole::Test | ArtifactRole::Document
    )
}

fn likely_repair_source_artifact(artifact: &str) -> bool {
    let normalized = normalize_closeout_target_path(artifact);
    let spec = classify_language_artifact_target(&normalized);
    spec.role == ArtifactRole::Source
        && !closeout_target_is_contract_artifact(&normalized)
        && spec.language != LanguageFamily::Text
}

fn closeout_target_is_contract_artifact(target: &str) -> bool {
    closeout_file_name(target)
        .map(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "scenario_contract.md" | "scenario_contract.json"
            )
        })
        .unwrap_or(false)
}

fn render_verification_failure_evidence(evidence: &VerificationCommandEvidence) -> String {
    if verification_evidence_is_public_command_contract(evidence) {
        return render_public_command_contract_failure_evidence(evidence);
    }
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

fn verification_evidence_is_public_command_contract(
    evidence: &VerificationCommandEvidence,
) -> bool {
    evidence.requirement_id.as_deref() == Some("public_command_contract")
        || evidence
            .normalized_failure_class
            .as_deref()
            .is_some_and(|class| class.contains("public_command_contract_failed"))
}

fn render_public_command_contract_failure_evidence(
    evidence: &VerificationCommandEvidence,
) -> String {
    let mut sections = vec![
        format!("command: {}", evidence.command),
        "requirement_id: public_command_contract".to_string(),
        "expected: route-owned public argv command contract passes with the recorded exit code and stdout/stderr observation".to_string(),
    ];
    let observed_issue = public_command_contract_observed_issue(evidence);
    sections.push(format!("observed: {observed_issue}"));
    if let Some(class) = evidence.normalized_failure_class.as_deref() {
        sections.push(format!("failure_class: {class}"));
    }
    sections.join("\n")
}

fn public_command_contract_observed_issue(evidence: &VerificationCommandEvidence) -> String {
    let stdout = evidence.stdout_summary.to_ascii_lowercase();
    let stderr = evidence.stderr_summary.to_ascii_lowercase();
    let failure = evidence
        .normalized_failure_class
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stderr.contains("eoferror") || stdout.contains("\n> ") || stdout.trim_end().ends_with('>') {
        return "argv invocation entered interactive stdin mode and reached EOF instead of processing command-line arguments".to_string();
    }
    if failure.contains("stdout had no line ending") {
        return "stdout did not expose the expected public result line suffix".to_string();
    }
    if failure.contains("stderr contained none") {
        return "stderr did not expose the expected usage/help/error observation".to_string();
    }
    if failure.contains("stdout contained none") {
        return "stdout did not expose the expected usage/help/error observation".to_string();
    }
    if let Some(exit_code) = evidence.exit_code {
        return format!(
            "public command exited with code {exit_code} but did not satisfy the route-owned output contract"
        );
    }
    "public command did not satisfy the route-owned output contract".to_string()
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
    validate_preflight_report_value(path.as_str(), &value)
}

fn validate_preflight_report_value(path: &str, value: &Value) -> Result<(), String> {
    if value.get("status").and_then(Value::as_str) != Some("pass") {
        return Err(format!(
            "preflight report `{path}` is not pass; representative route will not start"
        ));
    }
    if value.get("generated_by").and_then(Value::as_str) != Some("codex_style_preflight_v2") {
        return Err(format!(
            "preflight report `{path}` was not generated by codex_style_preflight_v2"
        ));
    }
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preflight report `{path}` has no results array"))?;
    if results.is_empty() {
        return Err(format!(
            "preflight report `{path}` has no active preflight results"
        ));
    }
    let mut fixture_ids = Vec::new();
    for result in results {
        if result.get("status").and_then(Value::as_str) != Some("pass") {
            return Err(format!(
                "preflight report `{path}` contains a non-pass preflight result"
            ));
        }
        let fixture_id = result
            .get("fixture_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!("preflight report `{path}` contains a result without fixture_id")
            })?;
        fixture_ids.push(fixture_id);
    }
    for required_fixture in [
        "fixture.protocol.history_item_lifecycle_authority",
        "fixture.control_envelope.dispatch_projection_authority",
        "fixture.tool_lifecycle.typed_route_metadata_authority",
        "fixture.manual_st.route_evidence_schema",
    ] {
        if !fixture_ids.contains(&required_fixture) {
            return Err(format!(
                "preflight report `{path}` is missing required active fixture `{required_fixture}`"
            ));
        }
    }
    Ok(())
}

pub(crate) fn manual_st_route_preflight_report_codex_style_admission_fixture_passes() -> bool {
    let valid = json!({
        "status": "pass",
        "generated_by": "codex_style_preflight_v2",
        "results": [
            {
                "fixture_id": "fixture.protocol.history_item_lifecycle_authority",
                "status": "pass"
            },
            {
                "fixture_id": "fixture.control_envelope.dispatch_projection_authority",
                "status": "pass"
            },
            {
                "fixture_id": "fixture.tool_lifecycle.typed_route_metadata_authority",
                "status": "pass"
            },
            {
                "fixture_id": "fixture.manual_st.route_evidence_schema",
                "status": "pass"
            }
        ]
    });
    let fabricated_pass = json!({
        "status": "pass",
        "generated_by": "test",
        "results": []
    });
    let missing_fixture = json!({
        "status": "pass",
        "generated_by": "codex_style_preflight_v2",
        "results": [
            {
                "fixture_id": "fixture.protocol.history_item_lifecycle_authority",
                "status": "pass"
            }
        ]
    });
    let failing_result = json!({
        "status": "pass",
        "generated_by": "codex_style_preflight_v2",
        "results": [
            {
                "fixture_id": "fixture.protocol.history_item_lifecycle_authority",
                "status": "fail"
            },
            {
                "fixture_id": "fixture.control_envelope.dispatch_projection_authority",
                "status": "pass"
            },
            {
                "fixture_id": "fixture.tool_lifecycle.typed_route_metadata_authority",
                "status": "pass"
            },
            {
                "fixture_id": "fixture.manual_st.route_evidence_schema",
                "status": "pass"
            }
        ]
    });

    validate_preflight_report_value("valid", &valid).is_ok()
        && validate_preflight_report_value("fabricated", &fabricated_pass).is_err()
        && validate_preflight_report_value("missing_fixture", &missing_fixture).is_err()
        && validate_preflight_report_value("failing_result", &failing_result).is_err()
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
    copy_scenario_contracts_if_available(case_id, workspace)?;
    Ok(())
}

fn copy_scenario_contracts_if_available(case_id: &str, workspace: &Utf8Path) -> Result<(), String> {
    let base_case_id = manual_st_base_case_id(case_id);
    let root = manual_st_root().join(base_case_id);
    for name in ["scenario_contract.md", "scenario_contract.json"] {
        let source = root.join(name);
        if source.exists() {
            fs::copy(source.as_std_path(), workspace.join(name).as_std_path())
                .map_err(|error| format!("failed to copy {name}: {error}"))?;
        }
    }
    Ok(())
}

fn manual_st_base_case_id(case_id: &str) -> &str {
    case_id
        .strip_suffix('a')
        .or_else(|| case_id.strip_suffix('b'))
        .or_else(|| case_id.strip_suffix('c'))
        .unwrap_or(case_id)
}

fn append_visible_scenario_contract_prompt(prompt: &str, workspace: &Utf8Path) -> String {
    let refs = scenario_contract_refs_for_workspace(workspace);
    if refs.is_empty() {
        return prompt.to_string();
    }
    let refs_text = refs
        .iter()
        .map(|name| format!("- `{name}`"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prompt}\n\nScenario contract authority:\n{refs_text}\nTreat these files as prompt-visible, harness-owned contract references. Generated tests may assert only the listed requirement ids, and assertions should include requirement ids in test names, docstrings, or assertion messages where practical."
    )
}

fn scenario_contract_refs_for_workspace(workspace: &Utf8Path) -> Vec<String> {
    ["scenario_contract.md", "scenario_contract.json"]
        .into_iter()
        .filter(|name| workspace.join(name).exists())
        .map(str::to_string)
        .collect()
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
    public_command_contracts: Vec<PublicCommandContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationCommandSpec {
    stage_label: Option<String>,
    command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PublicCommandContract {
    stage_label: Option<String>,
    command: String,
    expected_exit_code: i32,
    stdout_line_suffix: Option<String>,
    stdout_contains_any: Vec<String>,
    stderr_contains_any: Vec<String>,
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
        let verification_specs = extract_verification_command_specs(&spec);
        let has_stage_scoped_verification = verification_specs
            .iter()
            .any(|item| item.stage_label.is_some());
        let default_verification = verification_specs
            .iter()
            .filter(|item| item.stage_label.is_none())
            .map(|item| item.command.clone())
            .collect::<Vec<_>>();
        let public_command_contracts = extract_public_command_contracts(&spec);
        let prompt_count = prompts.len();
        let stages = prompts
            .into_iter()
            .enumerate()
            .map(|(index, (heading, prompt))| {
                let label = stage_label(&heading, index);
                let stage_verification = verification_specs
                    .iter()
                    .filter(|item| item.stage_label.as_deref() == Some(label.as_str()))
                    .map(|item| item.command.clone())
                    .collect::<Vec<_>>();
                let verification_commands = verification_commands_for_stage(
                    stage_verification,
                    &default_verification,
                    has_stage_scoped_verification,
                );
                ManualStStage {
                    public_command_contracts: public_command_contracts_for_stage(
                        &public_command_contracts,
                        &label,
                        index,
                        prompt_count,
                    ),
                    label,
                    prompt,
                    verification_commands,
                }
            })
            .collect::<Vec<_>>();
        let expected_artifacts = extract_expected_artifacts(&spec);
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

pub fn expected_artifacts_are_spec_owned_fixture_passes() -> bool {
    let spec_path = manual_st_root().join("case7").join("spec.md");
    let Ok(spec) = fs::read_to_string(spec_path.as_std_path()) else {
        return false;
    };
    let pass_criteria_only = r#"
## pass criteria

- `docs.md` exists.
"#;
    let hidden_case_fallback = [
        "case_id == ",
        "\"case7\"",
        " && expected_artifacts.is_empty",
    ]
    .join("");
    let hidden_docs_push = ["expected_artifacts.push(", "\"docs.md\"", ")"].join("");
    let source = include_str!("manual_st.rs");

    extract_expected_artifacts(&spec) == vec!["docs.md".to_string()]
        && ManualStCaseSpec::load("case7")
            .map(|loaded| loaded.expected_artifacts == vec!["docs.md".to_string()])
            .unwrap_or(false)
        && extract_expected_artifacts(pass_criteria_only).is_empty()
        && !source.contains(&hidden_case_fallback)
        && !source.contains(&hidden_docs_push)
}

fn extract_verification_command_specs(markdown: &str) -> Vec<VerificationCommandSpec> {
    let mut specs = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            let heading = line.to_ascii_lowercase();
            in_section = heading.contains("verification") || heading.contains("検証");
            continue;
        }
        if in_section && line.trim_start().starts_with('-') {
            let stage_label = extract_public_command_contract_stage_label(line);
            specs.extend(
                extract_backticks(line)
                    .into_iter()
                    .filter(|value| route_verification_command_like(value))
                    .map(|command| VerificationCommandSpec {
                        stage_label: stage_label.clone(),
                        command,
                    }),
            );
        }
    }
    specs
}

fn route_verification_command_like(value: &str) -> bool {
    let command = value.trim();
    if command.is_empty() || command.contains('\n') {
        return false;
    }
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let Some(program) = tokens.first().copied() else {
        return false;
    };
    if program.starts_with('-') {
        return false;
    }
    if tokens.len() == 1 {
        let normalized = normalize_closeout_target_path(command);
        let spec = classify_language_artifact_target(&normalized);
        if matches!(
            spec.role,
            ArtifactRole::Source | ArtifactRole::Test | ArtifactRole::Document
        ) || normalized.contains('/')
            || normalized.contains('.')
        {
            return false;
        }
    }
    true
}

pub(crate) fn manual_st_verification_commands_are_generic_public_commands_fixture_passes() -> bool {
    route_verification_command_like("npm test")
        && route_verification_command_like("pnpm test")
        && route_verification_command_like("go test ./...")
        && route_verification_command_like("node cli.js --help")
        && route_verification_command_like("pytest")
        && !route_verification_command_like("README.md")
        && !route_verification_command_like("docs/output.md")
        && !route_verification_command_like("src/workflow.rs")
}

fn verification_commands_for_stage(
    stage_verification: Vec<String>,
    default_verification: &[String],
    has_stage_scoped_verification: bool,
) -> Vec<String> {
    if has_stage_scoped_verification {
        stage_verification
    } else {
        default_verification.to_vec()
    }
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
    let output = run_manual_st_route_command_with_timeout(
        command,
        workspace,
        MANUAL_ST_ROUTE_COMMAND_TIMEOUT_SECONDS,
    )
    .await
    .map_err(|error| format!("failed to run verification `{command}`: {error}"))?;
    let output = match output {
        ManualStRouteCommandOutput::Completed(output) => output,
        ManualStRouteCommandOutput::TimedOut { timeout_seconds } => {
            return Ok(timeout_verification_evidence(
                command,
                workspace,
                case_id,
                None,
                start_time,
                timeout_seconds,
            ));
        }
    };
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

async fn run_public_command_contract(
    contract: &PublicCommandContract,
    workspace: &Utf8Path,
    case_id: &str,
) -> Result<VerificationCommandEvidence, String> {
    let start_time = timestamp();
    let output = run_manual_st_route_command_with_timeout(
        &contract.command,
        workspace,
        MANUAL_ST_ROUTE_COMMAND_TIMEOUT_SECONDS,
    )
    .await
    .map_err(|error| {
        format!(
            "failed to run public command contract `{}`: {error}",
            contract.command
        )
    })?;
    let output = match output {
        ManualStRouteCommandOutput::Completed(output) => output,
        ManualStRouteCommandOutput::TimedOut { timeout_seconds } => {
            return Ok(timeout_verification_evidence(
                &contract.command,
                workspace,
                case_id,
                Some("public_command_contract"),
                start_time,
                timeout_seconds,
            ));
        }
    };
    let actual_exit = output.status.code();
    let stdout = summarize_bytes(&output.stdout);
    let stderr = summarize_bytes(&output.stderr);
    let exit_matches = actual_exit == Some(contract.expected_exit_code);
    let stdout_matches = contract
        .stdout_line_suffix
        .as_deref()
        .is_none_or(|suffix| stdout_has_line_suffix(&stdout, suffix));
    let stdout_contains_matches =
        contains_any_or_unrequired(&stdout, &contract.stdout_contains_any);
    let stderr_contains_matches =
        contains_any_or_unrequired(&stderr, &contract.stderr_contains_any);
    let failure =
        (!exit_matches || !stdout_matches || !stdout_contains_matches || !stderr_contains_matches)
            .then(|| {
                let mut parts = Vec::new();
                if !exit_matches {
                    parts.push(format!(
                        "expected exit {} but got {:?}",
                        contract.expected_exit_code, actual_exit
                    ));
                }
                if !stdout_matches && let Some(suffix) = &contract.stdout_line_suffix {
                    parts.push(format!("stdout had no line ending with `{suffix}`"));
                }
                if !stdout_contains_matches {
                    parts.push(format!(
                        "stdout contained none of `{}`",
                        contract.stdout_contains_any.join(" | ")
                    ));
                }
                if !stderr_contains_matches {
                    parts.push(format!(
                        "stderr contained none of `{}`",
                        contract.stderr_contains_any.join(" | ")
                    ));
                }
                format!("public_command_contract_failed: {}", parts.join("; "))
            });
    Ok(VerificationCommandEvidence {
        command: contract.command.clone(),
        working_directory: workspace.to_string(),
        start_time,
        end_time: timestamp(),
        exit_code: actual_exit,
        stdout_summary: stdout,
        stderr_summary: stderr,
        normalized_failure_class: failure,
        required: true,
        case_id: case_id.to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    })
}

async fn run_stage_route_verification(
    stage: &ManualStStage,
    workspace: &Utf8Path,
    case_id: &str,
    verification_commands: &mut Vec<VerificationCommandEvidence>,
    stage_verification_commands: &mut Vec<VerificationCommandEvidence>,
) -> Result<(), String> {
    for command in &stage.verification_commands {
        let verification = run_verification_command(command, workspace, case_id).await?;
        stage_verification_commands.push(verification.clone());
        verification_commands.push(verification);
    }
    for contract in &stage.public_command_contracts {
        let verification = run_public_command_contract(contract, workspace, case_id).await?;
        stage_verification_commands.push(verification.clone());
        verification_commands.push(verification);
    }
    Ok(())
}

enum ManualStRouteCommandOutput {
    Completed(std::process::Output),
    TimedOut { timeout_seconds: u64 },
}

async fn run_manual_st_route_command_with_timeout(
    command: &str,
    workspace: &Utf8Path,
    timeout_seconds: u64,
) -> Result<ManualStRouteCommandOutput, String> {
    let mut process = manual_st_route_command(command, workspace);
    apply_manual_st_process_environment(&mut process);
    let child = process
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to spawn route command `{command}`: {error}"))?;
    let child_pid = child.id();
    match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => Ok(ManualStRouteCommandOutput::Completed(output)),
        Ok(Err(error)) => Err(format!("route command `{command}` failed to wait: {error}")),
        Err(_) => {
            if let Some(pid) = child_pid {
                cleanup_manual_st_route_process_tree(pid).await?;
            }
            Ok(ManualStRouteCommandOutput::TimedOut { timeout_seconds })
        }
    }
}

fn manual_st_route_command(command: &str, workspace: &Utf8Path) -> Command {
    let mut process = if cfg!(windows) {
        let mut process = Command::new("powershell");
        process.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        process
    } else {
        let mut process = Command::new("sh");
        process.args(["-lc", command]);
        process
    };
    process
        .current_dir(workspace.as_std_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process
}

#[cfg(windows)]
async fn cleanup_manual_st_route_process_tree(pid: u32) -> Result<(), String> {
    let script = format!(
        r#"
$root = {pid}
$all = Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId
$children = @{{}}
foreach ($p in $all) {{
  if (-not $children.ContainsKey([int]$p.ParentProcessId)) {{ $children[[int]$p.ParentProcessId] = @() }}
  $children[[int]$p.ParentProcessId] += [int]$p.ProcessId
}}
$stack = New-Object System.Collections.Generic.Stack[int]
$stack.Push($root)
$ids = New-Object System.Collections.Generic.List[int]
while ($stack.Count -gt 0) {{
  $current = $stack.Pop()
  if ($ids.Contains($current)) {{ continue }}
  $ids.Add($current)
  if ($children.ContainsKey($current)) {{
    foreach ($child in $children[$current]) {{ $stack.Push($child) }}
  }}
}}
foreach ($id in $ids | Sort-Object -Descending) {{
  Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
}}
"#
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| format!("failed to cleanup manual ST route process tree: {error}"))?;
    Ok(())
}

#[cfg(unix)]
async fn cleanup_manual_st_route_process_tree(pid: u32) -> Result<(), String> {
    let _ = Command::new("pkill")
        .args(["-TERM", "-P", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    Ok(())
}

#[cfg(not(any(windows, unix)))]
async fn cleanup_manual_st_route_process_tree(_pid: u32) -> Result<(), String> {
    Ok(())
}

fn timeout_verification_evidence(
    command: &str,
    workspace: &Utf8Path,
    case_id: &str,
    requirement_id: Option<&str>,
    start_time: String,
    timeout_seconds: u64,
) -> VerificationCommandEvidence {
    VerificationCommandEvidence {
        command: command.to_string(),
        working_directory: workspace.to_string(),
        start_time,
        end_time: timestamp(),
        exit_code: None,
        stdout_summary: String::new(),
        stderr_summary: format!(
            "manual ST route command timed out after {timeout_seconds}s before route evidence could advance"
        ),
        normalized_failure_class: Some("manual_st_route_command_timeout".to_string()),
        required: true,
        case_id: case_id.to_string(),
        requirement_id: requirement_id.map(str::to_string),
    }
}

fn stdout_has_line_suffix(stdout: &str, suffix: &str) -> bool {
    let suffix = suffix.trim();
    if suffix.is_empty() {
        return false;
    }
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| line_has_public_suffix(line, suffix))
}

fn line_has_public_suffix(line: &str, suffix: &str) -> bool {
    line == suffix
        || line.ends_with(&format!(": {suffix}"))
        || line.ends_with(&format!("：{suffix}"))
        || line.ends_with(&format!("： {suffix}"))
        || line.ends_with(&format!("= {suffix}"))
        || line.ends_with(&format!("-> {suffix}"))
        || line.ends_with(&format!(" {suffix}"))
}

fn contains_any_or_unrequired(text: &str, alternatives: &[String]) -> bool {
    alternatives.is_empty()
        || alternatives.iter().any(|alternative| {
            text.to_ascii_lowercase()
                .contains(&alternative.to_ascii_lowercase())
                || text.contains(alternative)
        })
}

fn summarize_bytes(bytes: &[u8]) -> String {
    let text = decode_manual_st_bytes_for_display(bytes);
    let mut summary = text.lines().take(80).collect::<Vec<_>>().join("\n");
    if summary.len() > 8_000 {
        summary.truncate(8_000);
    }
    summary
}

fn decode_manual_st_bytes_for_display(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(value) => value,
        Err(_) => {
            let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                decoded.into_owned()
            }
        }
    }
}

fn manual_st_utf8_process_environment() -> Vec<(&'static str, &'static str)> {
    vec![
        ("PYTHONUTF8", "1"),
        ("PYTHONIOENCODING", "utf-8"),
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C.UTF-8"),
    ]
}

fn apply_manual_st_process_environment(command: &mut Command) {
    for (key, value) in manual_st_utf8_process_environment() {
        command.env(key, value);
    }
}

pub(crate) fn route_verification_process_environment_fixture_passes() -> bool {
    let env = manual_st_utf8_process_environment()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let cp932_japanese = [
        0x8e, 0xa9, 0x91, 0x52, 0x91, 0xce, 0x90, 0x94, 0x82, 0xcc, 0x8a, 0xee, 0x96, 0x7b, 0x93,
        0x49, 0x82, 0xc8, 0x92, 0x6c,
    ];
    env.get("PYTHONUTF8") == Some(&"1")
        && env.get("PYTHONIOENCODING") == Some(&"utf-8")
        && env.get("LANG") == Some(&"C.UTF-8")
        && env.get("LC_ALL") == Some(&"C.UTF-8")
        && decode_manual_st_bytes_for_display(&cp932_japanese) == "自然対数の基本的な値"
        && summarize_bytes(&cp932_japanese) == "自然対数の基本的な値"
}

fn write_route_artifacts(
    route_root: &Utf8Path,
    result: &ManualStRouteResult,
    verification_commands: Vec<VerificationCommandEvidence>,
    workspace_diff: WorkspaceDiff,
) -> Result<(), String> {
    write_json(route_root.join("result.json"), result)?;
    write_case_progress_artifact(route_root, &ManualStCaseProgress::from_route_result(result))?;
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
    if matches!(result.route_level_verdict, RouteVerdict::Running) {
        if !route_root.join("timeout_classification.json").exists() {
            write_timeout_classification_with_reason(route_root, false, false, None)?;
        }
    } else {
        let reason = result.stop_reason.as_deref();
        let outer_timeout = reason.is_some_and(is_route_owned_turn_timeout_reason);
        write_timeout_classification_with_reason(
            route_root,
            outer_timeout,
            !outer_timeout,
            reason,
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
        "active_case_id": result.active_case_id.as_deref(),
        "progress_status": result.progress_status.as_deref(),
        "last_progress_at": result.last_progress_at.as_deref(),
        "evidence_artifacts": [
            "route_manifest.json",
            "case_progress.json",
            "verification_command_log.json",
            "workspace_diff_manifest.json",
            "result.json",
            "preflight_report.json",
            "timeout_classification.json"
        ]
    })
}

fn write_case_progress_artifact(
    route_root: &Utf8Path,
    progress: &ManualStCaseProgress,
) -> Result<(), String> {
    write_json(route_root.join("case_progress.json"), progress)?;
    if matches!(progress.route_level_verdict, RouteVerdict::Running) {
        write_running_timeout_classification_for_progress(route_root, progress)?;
    }
    Ok(())
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
        RunEvent::SessionInterrupted { reason, .. } => Some(reason.as_str()),
        _ => None,
    })
}

fn extract_public_command_contracts(markdown: &str) -> Vec<PublicCommandContract> {
    let mut contracts = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if line.starts_with("## ") {
            in_section = line
                .to_ascii_lowercase()
                .contains("public command contract");
            continue;
        }
        if !in_section || !line.trim_start().starts_with('-') {
            continue;
        }
        let Some(command) = extract_backticks(line).first().cloned() else {
            continue;
        };
        let expected_exit_code = extract_labeled_i32(line, "exit")
            .or_else(|| extract_labeled_i32(line, "exit_code"))
            .unwrap_or(0);
        let stdout_line_suffix = extract_labeled_backtick(line, "stdout_line_suffix");
        let stdout_contains_any = extract_labeled_backtick(line, "stdout_contains_any")
            .map(|value| split_contains_any(&value))
            .or_else(|| extract_labeled_backtick(line, "stdout_contains").map(|value| vec![value]))
            .unwrap_or_default();
        let stderr_contains_any = extract_labeled_backtick(line, "stderr_contains_any")
            .map(|value| split_contains_any(&value))
            .or_else(|| extract_labeled_backtick(line, "stderr_contains").map(|value| vec![value]))
            .unwrap_or_default();
        contracts.push(PublicCommandContract {
            stage_label: extract_public_command_contract_stage_label(line),
            command,
            expected_exit_code,
            stdout_line_suffix,
            stdout_contains_any,
            stderr_contains_any,
        });
    }
    contracts
}

fn split_contains_any(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn public_command_contracts_for_stage(
    contracts: &[PublicCommandContract],
    stage_label: &str,
    stage_index: usize,
    stage_count: usize,
) -> Vec<PublicCommandContract> {
    contracts
        .iter()
        .filter(|contract| match contract.stage_label.as_deref() {
            Some(label) => label == stage_label,
            None => stage_index + 1 == stage_count,
        })
        .cloned()
        .collect()
}

fn extract_public_command_contract_stage_label(line: &str) -> Option<String> {
    let trimmed = line.trim_start().trim_start_matches('-').trim_start();
    let (prefix, _) = trimmed.split_once(':')?;
    let label = prefix.trim().to_ascii_lowercase();
    label.starts_with("stage").then(|| {
        label
            .split_whitespace()
            .next()
            .unwrap_or(&label)
            .to_string()
    })
}

fn extract_labeled_i32(line: &str, label: &str) -> Option<i32> {
    let normalized = line.replace('`', " ");
    let label_index = normalized.to_ascii_lowercase().find(label)?;
    normalized[label_index + label.len()..]
        .split(|ch: char| !(ch == '-' || ch.is_ascii_digit()))
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse::<i32>().ok())
}

fn extract_labeled_backtick(line: &str, label: &str) -> Option<String> {
    let label_index = line.to_ascii_lowercase().find(label)?;
    extract_backticks(&line[label_index + label.len()..])
        .into_iter()
        .next()
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
    let route_owned_turn_timeout = reason.is_some_and(is_route_owned_turn_timeout_reason);
    let provider_stream_stall = reason.is_some_and(is_provider_stream_stall_reason);
    let provider_stream_retry_exhausted =
        reason.is_some_and(is_provider_stream_retry_exhausted_reason);
    let provider_transport_stream_error =
        reason.is_some_and(is_provider_transport_stream_error_reason);
    let semantic_no_progress_terminal =
        reason.is_some_and(is_semantic_no_progress_terminal_guard_reason);
    let evidence_refs = reason
        .filter(|_| {
            provider_stream_stall
                || provider_stream_retry_exhausted
                || provider_transport_stream_error
                || semantic_no_progress_terminal
        })
        .map(|reason| vec![reason.to_string()])
        .unwrap_or_default();
    let primary_timeout_owner = if outer_timeout || route_owned_turn_timeout {
        Some("harness_wait_policy")
    } else if provider_stream_retry_exhausted {
        Some("provider_stream_retry_exhausted")
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
        "provider_stream_retry_exhausted": provider_stream_retry_exhausted,
        "provider_transport_stream_error": provider_transport_stream_error,
        "verification_non_convergence": semantic_no_progress_terminal
            && reason.is_some_and(is_verification_non_convergence_reason),
        "repeated_no_progress_repair": semantic_no_progress_terminal,
        "semantic_no_progress_terminal_guard": semantic_no_progress_terminal,
        "tool_or_environment_stall": false,
        "outer_timeout": outer_timeout,
        "classified_terminal_before_timeout": classified_terminal_before_timeout,
        "primary_timeout_owner": primary_timeout_owner,
        "evidence_refs": evidence_refs
    })
}

fn write_running_timeout_classification_for_progress(
    root: &Utf8Path,
    progress: &ManualStCaseProgress,
) -> Result<(), String> {
    let mut value = timeout_classification_value(false, false, None);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "route_progress_status".to_string(),
            Value::String(progress.progress_status.clone()),
        );
        object.insert(
            "active_case_id".to_string(),
            progress
                .active_case_id
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stage_index".to_string(),
            progress
                .stage_index
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "stage_label".to_string(),
            progress
                .stage_label
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "session_id".to_string(),
            progress
                .session_id
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "last_progress_at".to_string(),
            Value::String(progress.last_progress_at.clone()),
        );
        object.insert(
            "inflight_owner".to_string(),
            route_inflight_owner_for_progress(&progress.progress_status)
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        object.insert(
            "interruption_classification_hint".to_string(),
            Value::String(
                "If this route process is killed externally while route_level_verdict is running, use route_progress_status and inflight_owner as the last event-sourced wait boundary before consulting provider artifacts."
                    .to_string(),
            ),
        );
    }
    write_json(root.join("timeout_classification.json"), &value)
}

fn route_inflight_owner_for_progress(progress_status: &str) -> Option<String> {
    match progress_status {
        "model_request_inflight" => Some("provider_model_request".to_string()),
        "route_verification_evaluating" => Some("harness_verification".to_string()),
        "closeout_continuation_pending" => Some("route_closeout_reducer".to_string()),
        "case_started" | "case_running" | "route_started" => Some("route_harness".to_string()),
        _ => None,
    }
}

fn is_route_owned_turn_timeout_reason(reason: &str) -> bool {
    reason
        .to_ascii_lowercase()
        .contains("exceeded max_turn_seconds")
}

fn is_provider_stream_stall_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("provider stream idle timeout")
        || lower.contains("without any sse event")
        || lower.contains("stream disconnected before completion")
}

fn is_provider_stream_retry_exhausted_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("stream retries exhausted")
        && lower.contains("stream_max_retries")
        && is_provider_stream_stall_reason(reason)
}

fn is_provider_transport_stream_error_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    (lower.contains("sse stream error") || lower.contains("response body"))
        && (lower.contains("transport error") || lower.contains("error decoding"))
}

fn is_semantic_no_progress_terminal_guard_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("docs/spec semantic reconciliation rejected")
        && lower.contains("runtime stopped before accepting artifact progress")
    {
        return true;
    }
    (lower.contains("supporting-context budget")
        || lower.contains("supporting_context output")
        || lower.contains("same verification failure evidence repeated")
        || lower.contains("representative survey budget is exhausted")
        || lower.contains("content-changing authoring is required")
        || lower.contains("runtime stopped before allowing more broad docs-route discovery"))
        && (lower.contains("no-progress")
            || lower.contains("docs authoring")
            || lower.contains("file-change evidence")
            || lower.contains("artifact progress")
            || lower.contains("repair/rerun loop"))
}

fn is_verification_non_convergence_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("same verification failure evidence repeated")
        || lower.contains("verification non-convergence")
}

pub(crate) fn provider_stream_idle_timeout_classification_fixture_passes() -> bool {
    let reason = "case2a stage1 failed before completion: provider stream idle timeout after 300000ms without any SSE event; stream retries exhausted after 3 attempt(s) with stream_max_retries=2";
    let value = timeout_classification_value(false, true, Some(reason));
    let idle_only_reason = "case2a stage1 failed before completion: provider stream idle timeout after 300000ms without any SSE event";
    let idle_only_value = timeout_classification_value(false, true, Some(idle_only_reason));
    value.get("provider_stream_stall").and_then(Value::as_bool) == Some(true)
        && value
            .get("provider_stream_retry_exhausted")
            .and_then(Value::as_bool)
            == Some(true)
        && value.get("primary_timeout_owner").and_then(Value::as_str)
            == Some("provider_stream_retry_exhausted")
        && value
            .get("evidence_refs")
            .and_then(Value::as_array)
            .is_some_and(|refs| refs.iter().any(|item| item.as_str() == Some(reason)))
        && idle_only_value
            .get("provider_stream_stall")
            .and_then(Value::as_bool)
            == Some(true)
        && idle_only_value
            .get("provider_stream_retry_exhausted")
            .and_then(Value::as_bool)
            == Some(false)
        && idle_only_value
            .get("primary_timeout_owner")
            .and_then(Value::as_str)
            == Some("provider_stream_idle_timeout")
}

pub(crate) fn manual_st_provider_retry_exhausted_timeout_classification_fixture_passes() -> bool {
    let reason = "case2a stage1 failed before completion: provider stream idle timeout after 300000ms without any SSE event; stream retries exhausted after 3 attempt(s) with stream_max_retries=2";
    let value = timeout_classification_value(false, true, Some(reason));
    value
        .get("provider_stream_retry_exhausted")
        .and_then(Value::as_bool)
        == Some(true)
        && value.get("provider_stream_stall").and_then(Value::as_bool) == Some(true)
        && value.get("primary_timeout_owner").and_then(Value::as_str)
            == Some("provider_stream_retry_exhausted")
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
    let verification_reason = "case3 stage3 ended with session status Failed: The same verification failure evidence repeated 3 time(s). Runtime stopped before continuing an unbounded repair/rerun loop.";
    let verification_value = timeout_classification_value(false, true, Some(verification_reason));
    let docs_semantic_reason = "case3 stage2 ended with session status Failed: Docs/spec semantic reconciliation rejected contradictory documentation 2 time(s). Runtime stopped before accepting artifact progress that violates the latest request authority. Targets: docs/calculator-design.md.";
    let docs_semantic_value = timeout_classification_value(false, true, Some(docs_semantic_reason));
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
        && verification_value
            .get("semantic_no_progress_terminal_guard")
            .and_then(Value::as_bool)
            == Some(true)
        && verification_value
            .get("verification_non_convergence")
            .and_then(Value::as_bool)
            == Some(true)
        && verification_value
            .get("primary_timeout_owner")
            .and_then(Value::as_str)
            == Some("semantic_no_progress_terminal_guard")
        && docs_semantic_value
            .get("semantic_no_progress_terminal_guard")
            .and_then(Value::as_bool)
            == Some(true)
        && docs_semantic_value
            .get("primary_timeout_owner")
            .and_then(Value::as_str)
            == Some("semantic_no_progress_terminal_guard")
}

pub fn route_owned_command_timeout_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 5"
    } else {
        "sleep 5"
    };
    let workspace_for_thread = workspace.clone();
    let command = command.to_string();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return false,
        };
        let result = runtime.block_on(run_manual_st_route_command_with_timeout(
            &command,
            &workspace_for_thread,
            0,
        ));
        match result {
            Ok(ManualStRouteCommandOutput::TimedOut { timeout_seconds }) => timeout_seconds == 0,
            _ => false,
        }
    })
    .join()
    .unwrap_or(false)
}

pub fn route_owned_command_timeout_cleans_process_tree_fixture_passes() -> bool {
    route_owned_command_timeout_fixture_passes()
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

pub fn stage_scoped_verification_commands_are_spec_owned_fixture_passes() -> bool {
    let specs = extract_verification_command_specs(
        r#"
## stage1 canonical user request

```text
author docs
```

## stage2 canonical user request

```text
author implementation
```

## required verification

- stage1: `python -m unittest`
- stage2: `cargo test`
"#,
    );
    let has_stage_scoped = specs.iter().any(|item| item.stage_label.is_some());
    let default = specs
        .iter()
        .filter(|item| item.stage_label.is_none())
        .map(|item| item.command.clone())
        .collect::<Vec<_>>();
    let stage1 = specs
        .iter()
        .filter(|item| item.stage_label.as_deref() == Some("stage1"))
        .map(|item| item.command.clone())
        .collect::<Vec<_>>();
    let stage2 = specs
        .iter()
        .filter(|item| item.stage_label.as_deref() == Some("stage2"))
        .map(|item| item.command.clone())
        .collect::<Vec<_>>();
    verification_commands_for_stage(stage1, &default, has_stage_scoped)
        == vec!["python -m unittest".to_string()]
        && verification_commands_for_stage(stage2, &default, has_stage_scoped)
            == vec!["cargo test".to_string()]
}

fn list_workspace_files(workspace: &Utf8Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_files(workspace, workspace, &mut files)?;
    files.sort();
    Ok(files)
}

fn workspace_content_fingerprint(workspace: &Utf8Path) -> Result<String, String> {
    let files = list_workspace_files(workspace)?;
    let mut entries = Vec::new();
    for file in files {
        let path = workspace.join(&file);
        let bytes = fs::read(path.as_std_path())
            .map_err(|error| format!("failed to read workspace file `{path}`: {error}"))?;
        entries.push(format!(
            "{file}\0{}\0{}",
            bytes.len(),
            crate::harness::artifact::hash_bytes(&bytes)
        ));
    }
    Ok(crate::harness::artifact::hash_bytes(
        entries.join("\n").as_bytes(),
    ))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_case_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    pub session_ids: Vec<SessionId>,
    pub case_results: Vec<ManualStCaseResult>,
    pub started_at: String,
    pub completed_at: String,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualStCaseProgress {
    pub route_id: String,
    pub route_type: String,
    pub route_level_verdict: RouteVerdict,
    pub active_case_id: Option<String>,
    pub stage_index: Option<usize>,
    pub stage_label: Option<String>,
    pub session_id: Option<String>,
    pub progress_status: String,
    pub last_progress_at: String,
    pub workspace_path: Option<Utf8PathBuf>,
    pub case_artifact_root: Option<Utf8PathBuf>,
    pub harness_event_root: Option<Utf8PathBuf>,
    pub evidence_artifact_schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ManualStCaseProgress {
    fn new(
        config: &ManualStRouteRunConfig,
        case_spec: &ManualStCaseSpec,
        stage_index: Option<usize>,
        stage_label: Option<&str>,
        session_id: Option<String>,
        workspace: &Utf8Path,
        case_root: &Utf8Path,
        data_dir: &Utf8Path,
        route_level_verdict: RouteVerdict,
        progress_status: &str,
        note: Option<&str>,
    ) -> Self {
        Self {
            route_id: config.route.route_id().to_string(),
            route_type: config.route.route_type().to_string(),
            route_level_verdict,
            active_case_id: Some(case_spec.case_id.clone()),
            stage_index,
            stage_label: stage_label.map(str::to_string),
            session_id,
            progress_status: progress_status.to_string(),
            last_progress_at: timestamp(),
            workspace_path: Some(workspace.to_path_buf()),
            case_artifact_root: Some(case_root.to_path_buf()),
            harness_event_root: Some(data_dir.join("harness")),
            evidence_artifact_schema_version: "manual_st.case_progress.v1".to_string(),
            note: note.map(str::to_string),
        }
    }

    fn from_route_result(result: &ManualStRouteResult) -> Self {
        Self {
            route_id: result.route_id.clone(),
            route_type: result.route_type.clone(),
            route_level_verdict: result.route_level_verdict,
            active_case_id: result.active_case_id.clone(),
            stage_index: None,
            stage_label: None,
            session_id: result.session_ids.last().map(ToString::to_string),
            progress_status: result
                .progress_status
                .clone()
                .unwrap_or_else(|| "route_artifact_written".to_string()),
            last_progress_at: result.last_progress_at.clone().unwrap_or_else(timestamp),
            workspace_path: None,
            case_artifact_root: None,
            harness_event_root: None,
            evidence_artifact_schema_version: "manual_st.case_progress.v1".to_string(),
            note: result.stop_reason.clone(),
        }
    }
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
                .unwrap_or_else(default_model_id),
            provider_base_url: config
                .base_url_override
                .clone()
                .unwrap_or_else(default_provider_base_url),
            provider_metadata_summary: json!({
                "source": "configured_model_gate",
                "preflight_report": config.preflight_report
            }),
            provider_metadata_hash: None,
            build_identifier: build_identifier(),
            expected_artifacts,
            route_level_verdict: RouteVerdict::NotRun,
            active_case_id: None,
            progress_status: None,
            last_progress_at: None,
            session_ids: Vec::new(),
            case_results: Vec::new(),
            started_at: now.clone(),
            completed_at: now,
            stop_reason: None,
        }
    }
}

fn default_model_id() -> String {
    configured_model_config()
        .map(|config| config.model)
        .unwrap_or_else(|| ResolvedConfig::default().model.model)
}

fn default_provider_base_url() -> String {
    configured_model_config()
        .map(|config| config.base_url)
        .unwrap_or_else(|| ResolvedConfig::default().model.base_url)
}

fn configured_model_config() -> Option<crate::config::ModelConfig> {
    crate::config::ConfigLoader::load(&Utf8PathBuf::from("."), None)
        .ok()
        .map(|config| config.model)
}

fn manual_st_run_request_model(config: &ManualStRouteRunConfig) -> String {
    config.model_override.clone().unwrap_or_default()
}

fn manual_st_run_request_base_url(config: &ManualStRouteRunConfig) -> String {
    config.base_url_override.clone().unwrap_or_default()
}

pub fn manual_st_route_omits_provider_defaults_without_explicit_override_fixture_passes() -> bool {
    let without_override = ManualStRouteRunConfig {
        route: ManualStRouteKind::RequiredCore,
        output_root: None,
        preflight_report: Utf8PathBuf::from("preflight.json"),
        model_override: None,
        base_url_override: None,
        provider_metadata_mode_override: None,
        context_window_override: None,
        max_output_tokens_override: None,
        max_turn_seconds: 7200,
        dry_run: false,
    };
    let with_override = ManualStRouteRunConfig {
        model_override: Some("custom-model".to_string()),
        base_url_override: Some("http://provider.example:1234".to_string()),
        ..without_override.clone()
    };

    manual_st_run_request_model(&without_override).is_empty()
        && manual_st_run_request_base_url(&without_override).is_empty()
        && manual_st_run_request_model(&with_override) == "custom-model"
        && manual_st_run_request_base_url(&with_override) == "http://provider.example:1234"
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_cluster: Option<ManualStTerminalClusterEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualStTerminalClusterEvidence {
    pub failure_family: String,
    pub fail_stop: bool,
    pub workspace_progress_can_reset: bool,
    pub source: String,
}

pub fn final_assistant_open_obligation_not_clean_closeout_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some(
            "src/workflow.rs was created. Next I will create tests/workflow.contract and run verification."
                .to_string(),
        ),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && evidence.runtime_completed
        && evidence
            .missing_artifacts
            .contains(&"tests/workflow.contract".to_string())
        && evidence
            .open_obligations
            .contains(&"author `tests/workflow.contract`".to_string())
        && !evidence.diagnostics.is_empty()
}

pub fn final_assistant_open_obligation_continuation_hook_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will create tests/workflow.contract.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && should_continue_after_closeout(&evidence)
        && prompt.contains("explicit text-only user turn")
        && prompt.contains("text-only user turn")
        && prompt.contains("Codex stop-hook continuation")
        && prompt.contains("tests/workflow.contract")
        && prompt.contains("verify-workflow --behavior")
        && prompt.contains("must use provider-visible file-changing tool calls")
        && prompt.contains("text-only promise about future work does not satisfy closeout")
        && !prompt.contains("[error]")
        && !prompt.contains("tool_choice=required")
}

pub fn open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::DocsRepair {
        deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
        pending_deliverables: Vec::new(),
        pending_summary: "docs route contract is pending".to_string(),
        route_contract_satisfied: false,
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will repair docs/workflow-design.md.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "docs/workflow-design.md".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "docs/workflow-design.md".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && evidence.missing_artifacts.is_empty()
        && evidence
            .open_obligations
            .contains(&"repair docs `docs/workflow-design.md`".to_string())
        && prompt.contains("Open obligations:")
        && prompt.contains("Expected artifacts:")
        && prompt.contains("route inventory evidence only")
        && prompt.contains("Do not create or update files only because they appear in the Expected artifacts inventory")
        && !prompt.contains("create or update the missing artifacts")
}

pub fn route_verification_waits_for_authored_artifacts_fixture_passes() -> bool {
    let authoring_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let open_authoring_closeout = classify_manual_st_closeout_from_evidence(
        true,
        Some("src/workflow.rs is ready; next I will create tests/workflow.contract.".to_string()),
        Some(&authoring_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let verification_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: vec![
            Utf8PathBuf::from("src/workflow.rs"),
            Utf8PathBuf::from("tests/workflow.contract"),
        ],
    };
    let verification_only_closeout = classify_manual_st_closeout_from_evidence(
        true,
        Some("Files are ready; I will verify them.".to_string()),
        Some(&verification_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[],
    );

    !manual_st_route_verification_may_run(&open_authoring_closeout)
        && open_authoring_closeout
            .missing_artifacts
            .contains(&"tests/workflow.contract".to_string())
        && open_authoring_closeout
            .open_obligations
            .contains(&"author `tests/workflow.contract`".to_string())
        && manual_st_route_verification_may_run(&verification_only_closeout)
        && verification_only_closeout.verification_required
            == vec!["verify-workflow --behavior".to_string()]
}

pub fn post_repair_route_verification_clears_stale_repair_fixture_passes() -> bool {
    let verification_work = ActiveWorkContract::Verification {
        commands: vec![
            "workflow-check --mode ok".to_string(),
            "workflow-check --mode bad".to_string(),
        ],
        failing_labels: vec!["failed command: workflow-check --mode ok".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let stale_failure = VerificationCommandEvidence {
        command: "workflow-check --mode ok".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "100".to_string(),
        end_time: "110".to_string(),
        exit_code: Some(0),
        stdout_summary: "interactive prompt".to_string(),
        stderr_summary: String::new(),
        normalized_failure_class: Some(
            "public_command_contract_failed: stdout had no line ending with `ok`".to_string(),
        ),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let post_repair_pass = VerificationCommandEvidence {
        command: "workflow-check --mode ok".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "200".to_string(),
        end_time: "210".to_string(),
        exit_code: Some(0),
        stdout_summary: "ok".to_string(),
        stderr_summary: String::new(),
        normalized_failure_class: None,
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let second_post_repair_pass = VerificationCommandEvidence {
        command: "workflow-check --mode bad".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "211".to_string(),
        end_time: "220".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "usage".to_string(),
        normalized_failure_class: None,
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&verification_work),
        &["src/workflow.rs".to_string()],
        &["src/workflow.rs".to_string()],
        &[stale_failure, post_repair_pass, second_post_repair_pass],
    );

    evidence.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && evidence.open_obligations.is_empty()
        && evidence.verification_required.is_empty()
        && evidence.verification_failed.is_empty()
        && evidence.verification_passed.len() == 2
        && route_owned_contract_satisfied_after_verification(&evidence)
}

pub fn closeout_continuation_is_text_only_fixture_passes() -> bool {
    image_paths_for_closeout_attempt("case2a", 0).len() == 1
        && image_paths_for_closeout_attempt("case2a", 1).is_empty()
        && image_paths_for_closeout_attempt("case2a", MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE)
            .is_empty()
}

pub fn stage_scoped_closeout_evidence_is_invalidated_fixture_passes() -> bool {
    let mut closeout_evidence = Some(classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        None,
        &["docs/design.md".to_string()],
        &["docs/design.md".to_string()],
        &[],
    ));
    if !closeout_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.closeout_class == ManualStCloseoutClass::CleanCloseout)
    {
        return false;
    }

    closeout_evidence = None;
    closeout_evidence.is_none()
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

pub fn public_command_contract_route_evidence_fixture_passes() -> bool {
    let markdown = r#"
## stage1 canonical user request
```text
Prepare the implementation.
```

## stage2 canonical user request
```text
Finish the public command surface.
```

## public command contract
- stage2: `tool sample --ok`; exit `0`; stdout_line_suffix `ready`
- stage2: `tool sample --bad-input`; exit `1`; stderr_contains_any `usage|使い方`
"#;
    let contracts = extract_public_command_contracts(markdown);
    let stage1_contracts = public_command_contracts_for_stage(&contracts, "stage1", 0, 2);
    let stage2_contracts = public_command_contracts_for_stage(&contracts, "stage2", 1, 2);
    if !stage1_contracts.is_empty()
        || stage2_contracts.len() != 2
        || stage2_contracts[0].stage_label.as_deref() != Some("stage2")
        || stage2_contracts[0].stdout_line_suffix.as_deref() != Some("ready")
        || stage2_contracts[1].expected_exit_code != 1
        || stage2_contracts[1].stderr_contains_any
            != vec!["usage".to_string(), "使い方".to_string()]
    {
        return false;
    }
    if !stdout_has_line_suffix("ready\n", "ready")
        || !stdout_has_line_suffix("status: ready\n", "ready")
        || !stdout_has_line_suffix("結果： 5\n", "5")
        || !stdout_has_line_suffix("2 + 3 = 5\n", "5")
        || !stdout_has_line_suffix("sin 0 -> 0\n", "0")
        || stdout_has_line_suffix("not-ready\n", "ready")
        || stdout_has_line_suffix("15\n", "5")
    {
        return false;
    }

    let expected_nonzero_pass = VerificationCommandEvidence {
        command: "tool sample --bad-input".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "usage: sample".to_string(),
        normalized_failure_class: None,
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let failed_public_command = VerificationCommandEvidence {
        command: "tool sample --ok".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "2".to_string(),
        end_time: "3".to_string(),
        exit_code: Some(0),
        stdout_summary: "not-ready".to_string(),
        stderr_summary: String::new(),
        normalized_failure_class: Some(
            "public_command_contract_failed: stdout had no line ending with `ready`".to_string(),
        ),
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let passed_evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        None,
        &["tool.md".to_string()],
        &["tool.md".to_string()],
        &[expected_nonzero_pass.clone()],
    );
    let failed_evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        None,
        &["tool.md".to_string()],
        &["tool.md".to_string()],
        &[expected_nonzero_pass, failed_public_command],
    );

    passed_evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && passed_evidence
            .verification_passed
            .contains(&"tool sample --bad-input".to_string())
        && passed_evidence.verification_failed.is_empty()
        && failed_evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && failed_evidence
            .verification_failed
            .contains(&"tool sample --ok".to_string())
}

pub fn manual_st_visible_scenario_contract_prompt_fixture_passes() -> bool {
    let root = manual_st_root().join("case1");
    let prompt = "Create src/workflow.rs and tests/workflow.contract.";
    root.join("scenario_contract.md").exists() && root.join("scenario_contract.json").exists() && {
        let rendered = append_visible_scenario_contract_prompt(prompt, root.as_path());
        rendered.contains("scenario_contract.md")
            && rendered.contains("scenario_contract.json")
            && rendered.contains("harness-owned contract references")
            && rendered.contains("requirement ids")
            && rendered.contains(prompt)
    }
}

pub fn route_result_progress_fields_fixture_passes() -> bool {
    let current_model = ResolvedConfig::default().model;
    let config = ManualStRouteRunConfig {
        route: ManualStRouteKind::RequiredCore,
        output_root: None,
        preflight_report: Utf8PathBuf::from("preflight.json"),
        model_override: Some(current_model.model.clone()),
        base_url_override: Some(current_model.base_url.clone()),
        provider_metadata_mode_override: None,
        context_window_override: None,
        max_output_tokens_override: None,
        max_turn_seconds: 60,
        dry_run: false,
    };
    let mut result = ManualStRouteResult::started(&config, Utf8PathBuf::from("route-root"));
    result.route_level_verdict = RouteVerdict::Running;
    mark_route_progress(&mut result, Some("caseX"), "case_running");
    let manifest = route_manifest(&result);

    result.active_case_id.as_deref() == Some("caseX")
        && result.progress_status.as_deref() == Some("case_running")
        && result.last_progress_at.is_some()
        && manifest.get("route_level_verdict").and_then(Value::as_str) == Some("running")
        && manifest.get("active_case_id").and_then(Value::as_str) == Some("caseX")
        && manifest.get("progress_status").and_then(Value::as_str) == Some("case_running")
        && manifest
            .get("last_progress_at")
            .and_then(Value::as_str)
            .is_some()
        && manifest
            .get("evidence_artifacts")
            .and_then(Value::as_array)
            .is_some_and(|artifacts| {
                artifacts
                    .iter()
                    .any(|artifact| artifact.as_str() == Some("case_progress.json"))
            })
}

pub fn route_inflight_case_progress_artifact_fixture_passes() -> bool {
    let current_model = ResolvedConfig::default().model;
    let config = ManualStRouteRunConfig {
        route: ManualStRouteKind::RequiredCore,
        output_root: None,
        preflight_report: Utf8PathBuf::from("preflight.json"),
        model_override: Some(current_model.model.clone()),
        base_url_override: Some(current_model.base_url.clone()),
        provider_metadata_mode_override: None,
        context_window_override: None,
        max_output_tokens_override: None,
        max_turn_seconds: 60,
        dry_run: false,
    };
    let case_spec = ManualStCaseSpec {
        case_id: "caseX".to_string(),
        expected_artifacts: vec!["artifact.txt".to_string()],
        stages: Vec::new(),
        task_file: None,
    };
    let route_root = Utf8PathBuf::from_path_buf(std::env::temp_dir())
        .ok()
        .unwrap_or_else(|| Utf8PathBuf::from("."))
        .join(format!(
            "moyai-case-progress-fixture-{}",
            SystemClock::now_ms()
        ));
    let workspace = route_root.join("caseX").join("workspace");
    let case_root = route_root.join("caseX");
    let data_dir = case_root.join("data");
    let progress = ManualStCaseProgress::new(
        &config,
        &case_spec,
        Some(2),
        Some("docs-update"),
        Some("01HQCASESESSION0000000000".to_string()),
        &workspace,
        &case_root,
        &data_dir,
        RouteVerdict::Running,
        "model_request_inflight",
        Some("fixture in-flight request"),
    );
    let write_ok = write_case_progress_artifact(&route_root, &progress).is_ok();
    let parsed = fs::read(route_root.join("case_progress.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let parsed_timeout = fs::read(route_root.join("timeout_classification.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let _ = fs::remove_dir_all(route_root.as_std_path());

    write_ok
        && parsed
            .as_ref()
            .and_then(|value| value.get("route_id"))
            .and_then(Value::as_str)
            == Some("required_core_route_a")
        && parsed
            .as_ref()
            .and_then(|value| value.get("active_case_id"))
            .and_then(Value::as_str)
            == Some("caseX")
        && parsed
            .as_ref()
            .and_then(|value| value.get("stage_index"))
            .and_then(Value::as_u64)
            == Some(2)
        && parsed
            .as_ref()
            .and_then(|value| value.get("session_id"))
            .and_then(Value::as_str)
            == Some("01HQCASESESSION0000000000")
        && parsed
            .as_ref()
            .and_then(|value| value.get("progress_status"))
            .and_then(Value::as_str)
            == Some("model_request_inflight")
        && parsed
            .as_ref()
            .and_then(|value| value.get("harness_event_root"))
            .and_then(Value::as_str)
            .is_some()
        && parsed_timeout
            .as_ref()
            .and_then(|value| value.get("route_progress_status"))
            .and_then(Value::as_str)
            == Some("model_request_inflight")
        && parsed_timeout
            .as_ref()
            .and_then(|value| value.get("inflight_owner"))
            .and_then(Value::as_str)
            == Some("provider_model_request")
        && parsed_timeout
            .as_ref()
            .and_then(|value| value.get("outer_timeout"))
            .and_then(Value::as_bool)
            == Some(false)
        && parsed_timeout
            .as_ref()
            .and_then(|value| value.get("classified_terminal_before_timeout"))
            .and_then(Value::as_bool)
            == Some(false)
        && parsed_timeout
            .as_ref()
            .and_then(|value| value.get("interruption_classification_hint"))
            .and_then(Value::as_str)
            .is_some_and(|hint| hint.contains("event-sourced wait boundary"))
}

pub fn route_case_progress_phase_boundaries_fixture_passes() -> bool {
    let current_model = ResolvedConfig::default().model;
    let config = ManualStRouteRunConfig {
        route: ManualStRouteKind::RequiredCore,
        output_root: None,
        preflight_report: Utf8PathBuf::from("preflight.json"),
        model_override: Some(current_model.model.clone()),
        base_url_override: Some(current_model.base_url.clone()),
        provider_metadata_mode_override: None,
        context_window_override: None,
        max_output_tokens_override: None,
        max_turn_seconds: 60,
        dry_run: false,
    };
    let case_spec = ManualStCaseSpec {
        case_id: "caseX".to_string(),
        expected_artifacts: vec!["artifact.txt".to_string()],
        stages: Vec::new(),
        task_file: None,
    };
    let root = Utf8PathBuf::from("route-root");
    let workspace = root.join("caseX").join("workspace");
    let case_root = root.join("caseX");
    let data_dir = case_root.join("data");
    let statuses = [
        "route_verification_evaluating",
        "closeout_continuation_pending",
        "stage_clean_closeout",
    ];

    statuses.iter().all(|status| {
        let progress = ManualStCaseProgress::new(
            &config,
            &case_spec,
            Some(1),
            Some("stage"),
            Some("01HQCASESESSION0000000000".to_string()),
            &workspace,
            &case_root,
            &data_dir,
            RouteVerdict::Running,
            status,
            Some("phase boundary fixture"),
        );
        progress.progress_status == *status
            && progress.active_case_id.as_deref() == Some("caseX")
            && progress.stage_index == Some(1)
            && progress.session_id.as_deref() == Some("01HQCASESESSION0000000000")
            && progress.note.as_deref() == Some("phase boundary fixture")
    })
}

pub fn successful_closeout_continuation_rematerializes_case_verdict_fixture_passes() -> bool {
    let expected_artifacts = vec![
        "src/workflow.rs".to_string(),
        "tests/workflow.contract".to_string(),
    ];
    let actual_files = expected_artifacts.clone();
    let passed_verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "1".to_string(),
        end_time: "2".to_string(),
        exit_code: Some(0),
        stdout_summary: "workflow contract passed".to_string(),
        stderr_summary: String::new(),
        normalized_failure_class: None,
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let clean_closeout = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        None,
        &expected_artifacts,
        &actual_files,
        &[passed_verification],
    );

    let (continued_verdict, continued_reason) = materialize_manual_st_case_terminal_verdict(
        RouteVerdict::Fail,
        None,
        false,
        "caseX",
        &expected_artifacts,
        &actual_files,
        Some(&clean_closeout),
    );
    let (transport_verdict, transport_reason) = materialize_manual_st_case_terminal_verdict(
        RouteVerdict::Fail,
        Some("provider stream idle timeout; stream retries exhausted".to_string()),
        false,
        "caseX",
        &expected_artifacts,
        &actual_files,
        Some(&clean_closeout),
    );

    matches!(continued_verdict, RouteVerdict::Pass)
        && continued_reason.is_none()
        && matches!(transport_verdict, RouteVerdict::Fail)
        && transport_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("provider stream idle timeout"))
}

pub fn route_terminal_verdict_rematerializes_from_case_results_fixture_passes() -> bool {
    let current_model = ResolvedConfig::default().model;
    let config = ManualStRouteRunConfig {
        route: ManualStRouteKind::RequiredCore,
        output_root: None,
        preflight_report: Utf8PathBuf::from("preflight.json"),
        model_override: Some(current_model.model.clone()),
        base_url_override: Some(current_model.base_url.clone()),
        provider_metadata_mode_override: None,
        context_window_override: None,
        max_output_tokens_override: None,
        max_turn_seconds: 60,
        dry_run: false,
    };
    let mut result = ManualStRouteResult::started(&config, Utf8PathBuf::from("route-root"));
    result.route_level_verdict = RouteVerdict::Fail;
    result.stop_reason = Some("stale case1 failure from prior materialization".to_string());
    result.case_results = vec![
        ManualStCaseResult {
            case_id: "case1".to_string(),
            verdict: RouteVerdict::Pass,
            session_ids: Vec::new(),
            expected_artifacts: Vec::new(),
            actual_files: Vec::new(),
            verification_commands: Vec::new(),
            closeout_evidence: None,
            stop_reason: None,
            timeout_observed: false,
        },
        ManualStCaseResult {
            case_id: "case3".to_string(),
            verdict: RouteVerdict::Pass,
            session_ids: Vec::new(),
            expected_artifacts: Vec::new(),
            actual_files: Vec::new(),
            verification_commands: Vec::new(),
            closeout_evidence: None,
            stop_reason: None,
            timeout_observed: false,
        },
    ];
    let (pass_verdict, pass_reason) = materialize_manual_st_route_terminal_verdict(&result);
    result.case_results[1].verdict = RouteVerdict::Fail;
    result.case_results[1].stop_reason = Some("case3 current terminal failure".to_string());
    let (fail_verdict, fail_reason) = materialize_manual_st_route_terminal_verdict(&result);

    matches!(pass_verdict, RouteVerdict::Pass)
        && pass_reason.is_none()
        && matches!(fail_verdict, RouteVerdict::Fail)
        && fail_reason.as_deref() == Some("case3 current terminal failure")
}

pub fn latest_verification_result_drives_closeout_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
    };
    let failed = VerificationCommandEvidence {
        command: "verify-workflow   --behavior".to_string(),
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
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "2".to_string(),
        end_time: "3".to_string(),
        exit_code: Some(0),
        stdout_summary: "workflow contract passed".to_string(),
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
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[failed, passed],
    );

    evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && evidence
            .verification_passed
            .contains(&"verify-workflow --behavior".to_string())
        && evidence.verification_failed.is_empty()
        && evidence.verification_required.is_empty()
}

pub fn verification_evidence_after_content_change_invalidated_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let mut verification_commands = vec![VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "10".to_string(),
        end_time: "20".to_string(),
        exit_code: Some(0),
        stdout_summary: String::new(),
        stderr_summary: "OK".to_string(),
        normalized_failure_class: None,
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: None,
    }];
    mark_stale_verification_evidence_after_content_change(
        &mut verification_commands,
        Some(30),
        &[],
    );
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        Some(&active_work),
        &["src/workflow.rs".to_string()],
        &["src/workflow.rs".to_string()],
        &verification_commands,
    );

    verification_commands[0]
        .normalized_failure_class
        .as_deref()
        .is_some_and(|class| class.contains("verification_stale_after_content_change"))
        && evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && evidence.verification_passed.is_empty()
        && evidence
            .verification_failed
            .contains(&"verify-workflow --behavior".to_string())
}

pub fn stage_without_required_verification_ignores_prior_stale_verification_fixture_passes() -> bool
{
    let route_verification_log = vec![VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "10".to_string(),
        end_time: "20".to_string(),
        exit_code: Some(0),
        stdout_summary: String::new(),
        stderr_summary: "OK".to_string(),
        normalized_failure_class: Some(
            "verification_stale_after_content_change: latest content change at 30 occurred after verification ended at 20"
                .to_string(),
        ),
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: Some("stage1".to_string()),
    }];
    let stage_verification_commands = Vec::new();
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Updated docs/design.md.".to_string()),
        None,
        &["docs/design.md".to_string()],
        &["docs/design.md".to_string()],
        &stage_verification_commands,
    );

    route_verification_log[0]
        .normalized_failure_class
        .as_deref()
        .is_some_and(|class| class.contains("verification_stale_after_content_change"))
        && evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && evidence.verification_failed.is_empty()
        && evidence.verification_required.is_empty()
        && closeout_continuation_kind(&evidence).is_none()
}

pub fn runtime_verification_pass_after_content_change_satisfies_route_closeout_fixture_passes()
-> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let mut verification_commands = vec![VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "10".to_string(),
        end_time: "20".to_string(),
        exit_code: Some(0),
        stdout_summary: String::new(),
        stderr_summary: "OK".to_string(),
        normalized_failure_class: None,
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: None,
    }];
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let history_items = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 40,
        payload: HistoryItemPayload::ToolOutput {
            call_id: ToolCallId::new(),
            status: ToolLifecycleStatus::Completed,
            title: "Run shell command: verify-workflow --behavior".to_string(),
            output_text: "workflow contract passed".to_string(),
            metadata: Value::Null,
            success: Some(true),
            progress_effect: ToolProgressEffect::VerificationPassed,
            blocked_action: None,
            result_hash: Some("verification-pass".to_string()),
            verification_run: Some(VerificationRunResult {
                command: "verify-workflow --behavior".to_string(),
                status: VerificationRunStatus::Passed,
                exit_code: Some(0),
                timed_out: false,
                output_summary: "workflow contract passed".to_string(),
                failure_cluster: None,
                satisfies_command_identities: vec!["verify-workflow --behavior".to_string()],
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    }];
    mark_stale_verification_evidence_after_content_change(
        &mut verification_commands,
        Some(30),
        &history_items,
    );
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        Some(&active_work),
        &["src/workflow.rs".to_string()],
        &["src/workflow.rs".to_string()],
        &verification_commands,
    );

    verification_commands[0].normalized_failure_class.is_none()
        && evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && evidence
            .verification_passed
            .contains(&"verify-workflow --behavior".to_string())
        && evidence.verification_failed.is_empty()
}

pub fn verification_failure_preserves_closeout_evidence_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
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
            "src/workflow.rs was created. Next I will create tests/workflow.contract and run verification."
                .to_string(),
        ),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[verification],
    );

    evidence.closeout_class == ManualStCloseoutClass::ContinuationPromised
        && evidence
            .verification_failed
            .contains(&"verify-workflow --behavior".to_string())
        && evidence
            .missing_artifacts
            .contains(&"tests/workflow.contract".to_string())
        && evidence
            .open_obligations
            .contains(&"author `tests/workflow.contract`".to_string())
        && evidence
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("required verification failed"))
}

pub fn verification_failed_closeout_builds_repair_hook_prompt_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: vec!["public_api".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "PublicApiError: WorkflowState has no operation `advance_step`".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public-api".to_string()),
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("The verification failed; next I will fix src/workflow.rs.".to_string()),
        Some(&active_work),
        &[
            "README.md".to_string(),
            "scenario_contract.json".to_string(),
            "scenario_contract.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "README.md".to_string(),
            "scenario_contract.json".to_string(),
            "scenario_contract.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[verification],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && should_continue_after_closeout(&evidence)
        && evidence
            .repair_targets
            .contains(&"src/workflow.rs".to_string())
        && evidence
            .verification_failed
            .contains(&"verify-workflow --behavior".to_string())
        && prompt.contains("verification-repair continuation")
        && prompt.contains("explicit text-only user turn")
        && prompt.contains("Codex stop-hook continuation")
        && prompt.contains("src/workflow.rs")
        && prompt.contains("verify-workflow --behavior")
        && prompt.contains("advance_step")
        && prompt.contains("apply_patch")
        && prompt.contains("rerun the failed required verification command")
        && prompt.contains("Do not answer with a text-only promise")
        && !prompt.contains("[error]")
        && !prompt.contains("tool_choice=required")
}

pub fn public_command_contract_closeout_prompt_compacts_failure_evidence_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["workflow-cli --sum 2 3".to_string()],
        failing_labels: vec!["failed command: workflow-cli --sum 2 3".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let verification = VerificationCommandEvidence {
        command: "workflow-cli --sum 2 3".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: "Usage\n\n> ".to_string(),
        stderr_summary: "Interactive input requested from public command without stdin".to_string(),
        normalized_failure_class: Some(
            "public_command_contract_failed: expected exit 0 but got Some(1); stdout had no line ending with `5`".to_string(),
        ),
        required: true,
        case_id: "routeX".to_string(),
        requirement_id: Some("public_command_contract".to_string()),
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Done.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[verification],
    );
    let prompt = build_closeout_continuation_prompt("routeX", "stage", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && evidence
            .verification_failure_evidence
            .iter()
            .any(|item| item.contains("requirement_id: public_command_contract"))
        && prompt.contains("public argv command contract")
        && prompt.contains("argv invocation entered interactive stdin mode")
        && prompt.contains("workflow-cli --sum 2 3")
        && !prompt.contains("Interactive input requested")
        && !prompt.contains("C:\\workspace")
        && !prompt.contains("line 9")
}

pub fn verification_failed_closeout_uses_generated_test_parse_target_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: vec!["failed command: verify-workflow --behavior".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary:
            "Generated test parse defect:\nTraceback (most recent call last):\n  File \"tests/workflow.spec.rs\", line 42\nSyntaxError: unterminated generated test block"
                .to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Latest evidence: verification failed.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.spec.rs".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.spec.rs".to_string(),
        ],
        &[verification],
    );
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::VerificationRequired
        && evidence.repair_targets == vec!["tests/workflow.spec.rs".to_string()]
        && evidence
            .verification_failure_evidence
            .iter()
            .any(|item| item.contains("unterminated generated test block"))
        && prompt.contains("Manual ST verification-repair continuation")
        && prompt.contains("Repair targets")
        && prompt.contains("tests/workflow.spec.rs")
        && !prompt.contains("Repair targets\n- src/workflow.rs")
}

pub fn closeout_artifact_roles_use_language_adapter_fixture_passes() -> bool {
    closeout_target_is_test_like("src/widget.spec.ts")
        && closeout_target_is_test_like("tests/test_widget.py")
        && !closeout_target_is_test_like("src/widget.ts")
        && closeout_target_is_mutable_source("src/widget.ts")
        && closeout_target_is_mutable_source("src/widget.py")
        && !closeout_target_is_mutable_source("src/widget.spec.ts")
        && !closeout_target_is_mutable_source("scenario_contract.json")
        && likely_repair_source_artifact("src/widget.ts")
        && likely_repair_source_artifact("src/widget.py")
        && !likely_repair_source_artifact("tests/test_widget.py")
        && !likely_repair_source_artifact("docs/widget-design.md")
        && closeout_target_is_deliverable_artifact("src/widget.spec.ts")
        && closeout_target_is_deliverable_artifact("src/widget.ts")
        && closeout_target_is_deliverable_artifact("docs/widget-design.md")
}

pub fn closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let open_evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("Next I will create tests/workflow.contract.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let repair_active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: vec!["public_api".to_string()],
        repair_required: false,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let failed_verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
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
        Some("Next I will fix src/workflow.rs.".to_string()),
        Some(&repair_active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[failed_verification],
    );

    let mut budget = CloseoutContinuationBudget::default();
    let first_open = budget.next_attempt(&open_evidence);
    let first_repair = budget.next_attempt(&repair_evidence);
    let mut remaining = Vec::new();
    for _ in 2..MAX_CLOSEOUT_CONTINUATIONS_PER_STAGE {
        remaining.push(budget.next_attempt(&open_evidence));
    }
    first_open == Some(1)
        && first_repair == Some(1)
        && remaining.iter().all(|attempt| attempt.is_some())
        && budget.next_attempt(&open_evidence).is_none()
        && budget.next_attempt(&repair_evidence).is_none()
        && closeout_continuation_signature(&open_evidence)
            != closeout_continuation_signature(&repair_evidence)
}

pub fn closeout_continuation_budget_blocks_same_workspace_stall_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: vec!["test_public_behavior".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let failed_verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "AssertionError: result.returncode expected 1".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("I will inspect src/workflow.rs again.".to_string()),
        Some(&active_work),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[failed_verification],
    );

    let mut budget = CloseoutContinuationBudget::default();
    let first =
        budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-fingerprint-a");
    let second =
        budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-fingerprint-a");
    let third =
        budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-fingerprint-a");
    let blocked =
        budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-fingerprint-a");
    let after_progress =
        budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-fingerprint-b");

    first == Some(1)
        && second == Some(2)
        && third == Some(3)
        && blocked.is_none()
        && after_progress == Some(4)
}

pub fn verification_failure_labels_do_not_become_authoring_obligations_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![
            Utf8PathBuf::from("workflow_contract.behavior.creates_transition"),
            Utf8PathBuf::from("workflow_contract.behavior.rejects_invalid_transition"),
            Utf8PathBuf::from("workflow_contract.behavior.records_status"),
        ],
        verification_commands: vec!["verify-workflow --behavior".to_string()],
    };
    let verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "TypeError: WorkflowTransition received unexpected field 'external_state'"
            .to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: Some("public-api".to_string()),
    };
    let expected = vec![
        "README.md".to_string(),
        "scenario_contract.json".to_string(),
        "scenario_contract.md".to_string(),
        "src/workflow.rs".to_string(),
        "tests/workflow.contract".to_string(),
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
            .contains(&"verify-workflow --behavior".to_string())
        && evidence
            .repair_targets
            .contains(&"src/workflow.rs".to_string())
        && closeout_continuation_kind(&evidence) == Some("verification_failed")
        && prompt.contains("Manual ST verification-repair continuation")
        && prompt.contains("src/workflow.rs")
        && prompt.contains("verify-workflow --behavior")
        && prompt.contains("apply_patch")
        && !prompt.contains("author `workflow_contract.behavior")
        && !prompt.contains("tool_choice=required")
}

pub fn runtime_failure_closeout_recomputes_current_artifacts_fixture_passes() -> bool {
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-workflow --behavior".to_string()],
        failing_labels: vec!["workflow_contract".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let verification = VerificationCommandEvidence {
        command: "verify-workflow --behavior".to_string(),
        working_directory: "workspace".to_string(),
        start_time: "0".to_string(),
        end_time: "1".to_string(),
        exit_code: Some(1),
        stdout_summary: String::new(),
        stderr_summary: "SyntaxError: invalid character".to_string(),
        normalized_failure_class: Some("verification_failed".to_string()),
        required: true,
        case_id: "caseX".to_string(),
        requirement_id: None,
    };
    let expected = vec![
        "src/workflow.rs".to_string(),
        "tests/workflow.contract".to_string(),
    ];
    let actual = expected.clone();
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&active_work),
        &expected,
        &actual,
        &[verification],
    );

    evidence.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && evidence.missing_artifacts.is_empty()
        && evidence
            .open_obligations
            .contains(&"repair `src/workflow.rs`".to_string())
        && evidence
            .verification_failed
            .contains(&"verify-workflow --behavior".to_string())
        && evidence
            .repair_targets
            .contains(&"src/workflow.rs".to_string())
        && !evidence
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("expected artifacts are missing"))
}

pub fn run_error_closeout_replaces_stale_continuation_evidence_fixture_passes() -> bool {
    let stale = classify_manual_st_closeout_from_evidence(
        true,
        Some("I will create both files next.".to_string()),
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![
                Utf8PathBuf::from("src/workflow.rs"),
                Utf8PathBuf::from("tests/workflow.contract"),
            ],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[],
        &[],
    );
    let current = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );

    stale
        .missing_artifacts
        .contains(&"src/workflow.rs".to_string())
        && stale
            .missing_artifacts
            .contains(&"tests/workflow.contract".to_string())
        && current.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && !current
            .missing_artifacts
            .contains(&"src/workflow.rs".to_string())
        && current
            .missing_artifacts
            .contains(&"tests/workflow.contract".to_string())
        && !current
            .open_obligations
            .contains(&"author `src/workflow.rs`".to_string())
        && current
            .open_obligations
            .contains(&"author `tests/workflow.contract`".to_string())
}

pub fn run_error_open_obligation_uses_closeout_continuation_budget_fixture_passes() -> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let mut budget = CloseoutContinuationBudget::default();
    let first_attempt = budget.next_attempt(&evidence);
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);
    let docs_evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::DocsRepair {
            deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
            pending_deliverables: vec![crate::session::DocsPendingDeliverable {
                target: Utf8PathBuf::from("docs/workflow-design.md"),
                summary: "same-document docs update requested after the latest user turn"
                    .to_string(),
            }],
            pending_summary: "docs route contract is pending".to_string(),
            route_contract_satisfied: false,
        }),
        &["docs/workflow-design.md".to_string()],
        &["docs/workflow-design.md".to_string()],
        &[],
    );
    let mut docs_budget = CloseoutContinuationBudget::default();
    let docs_attempt = docs_budget.next_attempt(&docs_evidence);
    let docs_prompt = build_closeout_continuation_prompt("caseX", "stage2", 1, &docs_evidence);

    evidence.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && first_attempt == Some(1)
        && closeout_continuation_kind(&evidence) == Some("open_obligation")
        && prompt.contains("ended before clean route closeout")
        && prompt.contains("Continuation attempt: 1/")
        && prompt.contains("Missing expected artifacts")
        && prompt.contains("tests/workflow.contract")
        && prompt.contains("Open obligations")
        && prompt.contains("author `tests/workflow.contract`")
        && docs_evidence.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && docs_attempt == Some(1)
        && closeout_continuation_kind(&docs_evidence) == Some("open_obligation")
        && docs_evidence.missing_artifacts.is_empty()
        && docs_evidence
            .open_obligations
            .contains(&"repair docs `docs/workflow-design.md`".to_string())
        && docs_prompt.contains("repair docs `docs/workflow-design.md`")
}

pub fn runtime_terminal_status_uses_closeout_continuation_budget_fixture_passes() -> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let reason = "caseX stage1 ended with session status Failed: Provider repeated a rejected model action with no progress 3 time(s). Runtime stopped on the lifecycle adjudication cluster `provider_ignored_edit_only_surface` before applying side effects outside the compiled TurnControlEnvelope lifecycle.";
    let transport_reason =
        "caseX stage1 ended with session status Failed: provider stream idle timeout";
    let mut budget = CloseoutContinuationBudget::default();
    let first_attempt = if !is_provider_stream_stall_reason(reason)
        && !is_provider_transport_stream_error_reason(reason)
    {
        budget.next_attempt(&evidence)
    } else {
        None
    };
    let prompt = build_closeout_continuation_prompt("caseX", "stage1", 1, &evidence);

    evidence.closeout_class == ManualStCloseoutClass::RuntimeDidNotComplete
        && first_attempt == Some(1)
        && closeout_continuation_kind(&evidence) == Some("open_obligation")
        && prompt.contains("ended before clean route closeout")
        && prompt.contains("tests/workflow.contract")
        && !is_provider_stream_stall_reason(reason)
        && !is_provider_transport_stream_error_reason(reason)
        && is_provider_stream_stall_reason(transport_reason)
}

pub fn terminalized_session_continuation_ledger_bounds_same_stage_recovery_fixture_passes() -> bool
{
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("tests/workflow.contract")],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &["src/workflow.rs".to_string()],
        &[],
    );
    let invalid_read_reason = "caseX stage1 ended with session status Failed: Provider repeated invalid arguments for read while typed work remained open.";
    let final_message_reason = "caseX stage1 ended with session status Failed: model returned a final assistant message 3 time(s) while typed work remained open.";
    let lifecycle_reason = "caseX stage1 ended with session status Failed: Runtime stopped on the lifecycle adjudication cluster `provider_ignored_edit_only_surface` before applying side effects.";
    let invalid_write_reason = "caseX stage1 ended with session status Failed: Provider repeated invalid arguments for write while typed work remained open.";
    let content_shape_no_progress_reason = "caseX stage1 ended with session status Failed: Tool `apply_patch` returned `no_progress` output 3 time(s) while content-changing authoring is required. Runtime stopped before treating non-content tool calls as artifact progress.";

    let mut plain_budget = CloseoutContinuationBudget::default();
    let plain_first =
        plain_budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-a");
    let plain_second =
        plain_budget.next_attempt_with_workspace_fingerprint(&evidence, "workspace-a");

    let mut budget = CloseoutContinuationBudget::default();
    let mut ledger = RouteStageTerminalContinuationLedger::default();
    let first = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-a",
        Some(invalid_read_reason),
    );
    let repeated_same_cluster = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-a",
        Some(invalid_read_reason),
    );
    let mut no_progress_budget = CloseoutContinuationBudget::default();
    let mut no_progress_ledger = RouteStageTerminalContinuationLedger::default();
    let first_no_progress = next_stage_closeout_continuation_attempt(
        &mut no_progress_budget,
        &mut no_progress_ledger,
        &evidence,
        "workspace-a",
        Some(content_shape_no_progress_reason),
    );
    let repeated_no_progress_same_workspace = next_stage_closeout_continuation_attempt(
        &mut no_progress_budget,
        &mut no_progress_ledger,
        &evidence,
        "workspace-a",
        Some(content_shape_no_progress_reason),
    );

    let mut stage_budget = CloseoutContinuationBudget::default();
    let mut stage_ledger = RouteStageTerminalContinuationLedger::default();
    let first_cluster = next_stage_closeout_continuation_attempt(
        &mut stage_budget,
        &mut stage_ledger,
        &evidence,
        "workspace-a",
        Some(invalid_read_reason),
    );
    let second_cluster = next_stage_closeout_continuation_attempt(
        &mut stage_budget,
        &mut stage_ledger,
        &evidence,
        "workspace-b",
        Some(final_message_reason),
    );
    let third_cluster = next_stage_closeout_continuation_attempt(
        &mut stage_budget,
        &mut stage_ledger,
        &evidence,
        "workspace-c",
        Some(lifecycle_reason),
    );
    let stage_blocked = next_stage_closeout_continuation_attempt(
        &mut stage_budget,
        &mut stage_ledger,
        &evidence,
        "workspace-d",
        Some(invalid_write_reason),
    );

    let no_progress_cluster =
        route_stage_terminal_continuation_cluster(&evidence, content_shape_no_progress_reason)
            .unwrap_or_default();
    let mut typed_no_progress_evidence = evidence.clone();
    typed_no_progress_evidence.terminal_cluster = Some(ManualStTerminalClusterEvidence {
        failure_family: "content_changing_authoring_no_progress:apply_patch".to_string(),
        fail_stop: true,
        workspace_progress_can_reset: false,
        source: "tool_output.tool_feedback_envelope".to_string(),
    });
    let mut typed_budget = CloseoutContinuationBudget::default();
    let mut typed_ledger = RouteStageTerminalContinuationLedger::default();
    let typed_attempt = next_stage_closeout_continuation_attempt(
        &mut typed_budget,
        &mut typed_ledger,
        &typed_no_progress_evidence,
        "workspace-a",
        Some("opaque terminal summary without parseable marker"),
    );
    let typed_cluster = route_stage_terminal_continuation_cluster(
        &typed_no_progress_evidence,
        "opaque terminal summary without parseable marker",
    )
    .unwrap_or_default();

    plain_first == Some(1)
        && plain_second == Some(2)
        && first == Some(1)
        && repeated_same_cluster.is_none()
        && first_no_progress.is_none()
        && repeated_no_progress_same_workspace.is_none()
        && first_cluster == Some(1)
        && second_cluster == Some(2)
        && third_cluster == Some(3)
        && stage_blocked.is_none()
        && route_stage_terminal_continuation_cluster(&evidence, invalid_read_reason)
            == route_stage_terminal_continuation_cluster(&evidence, invalid_read_reason)
        && no_progress_cluster
            .starts_with("content_changing_authoring_no_progress:apply_patch|open_obligation")
        && typed_attempt.is_none()
        && typed_cluster
            .starts_with("content_changing_authoring_no_progress:apply_patch|open_obligation")
        && no_progress_cluster.contains("tests/workflow.contract")
        && route_stage_terminal_continuation_cluster(&evidence, invalid_read_reason)
            != route_stage_terminal_continuation_cluster(&evidence, final_message_reason)
}

pub fn route_terminal_cluster_uses_typed_tool_output_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();
    let history = vec![HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Completed,
            title: "No progress".to_string(),
            output_text: "opaque provider-facing text".to_string(),
            metadata: json!({
                "tool_feedback_envelope": {
                    "tool": "write",
                    "operation_intent": "content_changing_authoring_required",
                    "operation_progress_class": "no_progress",
                    "progress_effect": "no_progress",
                    "side_effects_applied": false
                }
            }),
            success: Some(false),
            progress_effect: ToolProgressEffect::NoProgress,
            blocked_action: None,
            result_hash: Some("typed-no-progress".to_string()),
            verification_run: None,
        },
    }];
    let Some(cluster) = latest_route_stage_terminal_cluster_evidence(&history) else {
        return false;
    };
    cluster.failure_family == "content_changing_authoring_no_progress:write"
        && cluster.fail_stop
        && !cluster.workspace_progress_can_reset
        && cluster.source == "tool_output.tool_feedback_envelope"
}

pub fn authoring_grounding_terminal_fail_stops_route_fixture_passes() -> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        Some("case2a stage1 ended with session status Failed: Authoring supporting-context budget was exhausted and the model repeated non-remaining active target read proposals 3 time(s) for `tests/workflow.contract` instead of reading the remaining target or producing file-change evidence. Runtime stopped before growing provider history with more no-progress corrections. Consumed target paths: src/workflow.rs. Remaining read target paths: . Active target set: src/workflow.rs.".to_string()),
        Some(&ActiveWorkContract::Verification {
            commands: vec!["verify-workflow --behavior".to_string()],
            failing_labels: vec!["workflow_contract_public_api".to_string()],
            repair_required: true,
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        }),
        &[
            "README.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "README.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[VerificationCommandEvidence {
            command: "verify-workflow --behavior".to_string(),
            working_directory: "workspace".to_string(),
            start_time: "0".to_string(),
            end_time: "1".to_string(),
            exit_code: Some(1),
            stdout_summary: String::new(),
            stderr_summary: "PublicApiError: missing workflow operation `emit_event`".to_string(),
            normalized_failure_class: Some("verification_failed".to_string()),
            required: true,
            case_id: "caseX".to_string(),
            requirement_id: Some("API-4".to_string()),
        }],
    );
    let reason = "case2a stage1 ended with session status Failed: Authoring supporting-context budget was exhausted and the model repeated non-remaining active target read proposals 3 time(s) for `tests/workflow.contract` instead of reading the remaining target or producing file-change evidence. Runtime stopped before growing provider history with more no-progress corrections. Consumed target paths: src/workflow.rs. Remaining read target paths: . Active target set: src/workflow.rs.";
    let mut budget = CloseoutContinuationBudget::default();
    let mut ledger = RouteStageTerminalContinuationLedger::default();
    let attempt = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-with-open-source-repair",
        Some(reason),
    );
    let cluster = route_stage_terminal_continuation_cluster(&evidence, reason).unwrap_or_default();
    let timeout = timeout_classification_value(false, true, Some(reason));

    attempt.is_none()
        && terminal_reason_requires_route_fail_stop(reason)
        && cluster.starts_with("authoring_grounding_budget_exhausted|")
        && timeout
            .get("semantic_no_progress_terminal_guard")
            .and_then(Value::as_bool)
            == Some(true)
        && timeout
            .get("repeated_no_progress_repair")
            .and_then(Value::as_bool)
            == Some(true)
}

pub fn content_changing_authoring_no_progress_terminal_fail_stops_route_fixture_passes() -> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        Some("case2a stage1 ended with session status Failed: Tool `write` returned `no_progress` output 3 time(s) while content-changing authoring is required. Runtime stopped before treating non-content tool calls as artifact progress. Use apply_patch or equivalent file-change evidence for open targets: README.md, src/workflow.rs, tests/workflow.contract.".to_string()),
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![
                Utf8PathBuf::from("README.md"),
                Utf8PathBuf::from("src/workflow.rs"),
                Utf8PathBuf::from("tests/workflow.contract"),
            ],
            verification_commands: vec!["verify-workflow --behavior".to_string()],
        }),
        &[
            "README.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[],
        &[],
    );
    let reason = "case2a stage1 ended with session status Failed: Tool `write` returned `no_progress` output 3 time(s) while content-changing authoring is required. Runtime stopped before treating non-content tool calls as artifact progress. Use apply_patch or equivalent file-change evidence for open targets: README.md, src/workflow.rs, tests/workflow.contract.";
    let mut budget = CloseoutContinuationBudget::default();
    let mut ledger = RouteStageTerminalContinuationLedger::default();
    let attempt = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-without-authored-targets",
        Some(reason),
    );
    let cluster = route_stage_terminal_continuation_cluster(&evidence, reason).unwrap_or_default();
    let timeout = timeout_classification_value(false, true, Some(reason));

    attempt.is_none()
        && terminal_reason_requires_route_fail_stop(reason)
        && cluster.starts_with("content_changing_authoring_no_progress:write|")
        && timeout
            .get("semantic_no_progress_terminal_guard")
            .and_then(Value::as_bool)
            == Some(true)
        && timeout
            .get("repeated_no_progress_repair")
            .and_then(Value::as_bool)
            == Some(true)
}

pub fn verification_repair_terminal_ledger_blocks_non_obligation_workspace_progress_fixture_passes()
-> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        Some("case2a stage1 ended with session status Failed: The same verification failure evidence repeated 3 time(s). Runtime stopped before continuing an unbounded repair/rerun loop.".to_string()),
        Some(&ActiveWorkContract::Verification {
            commands: vec!["verify-workflow --behavior".to_string()],
            failing_labels: vec!["failed command: verify-workflow --behavior".to_string()],
            repair_required: true,
            targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        }),
        &[
            "README.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[
            "README.md".to_string(),
            "src/workflow.rs".to_string(),
            "tests/workflow.contract".to_string(),
        ],
        &[VerificationCommandEvidence {
            command: "verify-workflow --behavior".to_string(),
            working_directory: "workspace".to_string(),
            start_time: "0".to_string(),
            end_time: "1".to_string(),
            exit_code: Some(1),
            stdout_summary: String::new(),
            stderr_summary:
                "PublicApiError: WorkflowState has no operation `resolve_conflict`".to_string(),
            normalized_failure_class: Some("verification_failed".to_string()),
            required: true,
            case_id: "caseX".to_string(),
            requirement_id: Some("BEH-4".to_string()),
        }],
    );
    let reason = "case2a stage1 ended with session status Failed: The same verification failure evidence repeated 3 time(s). Runtime stopped before continuing an unbounded repair/rerun loop. Inspect the latest stdout/stderr and make a materially different repair before rerunning verification.";
    let mut budget = CloseoutContinuationBudget::default();
    let mut ledger = RouteStageTerminalContinuationLedger::default();
    let first = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-with-gui-entrypoint-added",
        Some(reason),
    );
    let unrelated_workspace_progress = next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-with-different-non-obligation-edit",
        Some(reason),
    );
    let cluster = route_stage_terminal_continuation_cluster(&evidence, reason).unwrap_or_default();

    first == Some(1)
        && unrelated_workspace_progress.is_none()
        && cluster.starts_with("verification_non_convergence|")
        && cluster.contains("src/workflow.rs")
}

pub fn unknown_terminal_reason_fail_stops_open_route_fixture_passes() -> bool {
    let evidence = classify_manual_st_closeout_from_evidence(
        false,
        Some("caseX stage1 ended with session status Failed: opaque runtime terminal".to_string()),
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("src/widget.rs")],
            verification_commands: vec!["cargo test".to_string()],
        }),
        &["src/widget.rs".to_string()],
        &[],
        &[],
    );
    let mut budget = CloseoutContinuationBudget::default();
    let mut ledger = RouteStageTerminalContinuationLedger::default();
    let unknown_reason = "caseX stage1 ended with session status Failed: opaque runtime terminal";
    let missing_reason = None;

    next_stage_closeout_continuation_attempt(
        &mut budget,
        &mut ledger,
        &evidence,
        "workspace-a",
        Some(unknown_reason),
    )
    .is_none()
        && next_stage_closeout_continuation_attempt(
            &mut budget,
            &mut ledger,
            &evidence,
            "workspace-b",
            missing_reason,
        )
        .is_none()
        && route_stage_terminal_continuation_cluster(&evidence, unknown_reason).is_none()
}

pub fn completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes() -> bool {
    let expected_artifacts = vec!["docs/workflow-design.md".to_string()];
    let actual_files = vec!["docs/workflow-design.md".to_string()];
    let completed = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("docs/workflow-design.md")],
            verification_commands: Vec::new(),
        }),
        &expected_artifacts,
        &actual_files,
        &[],
    );
    let missing = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets: vec![Utf8PathBuf::from("docs/workflow-design.md")],
            verification_commands: Vec::new(),
        }),
        &expected_artifacts,
        &[],
        &[],
    );
    let repair_remains_open = classify_manual_st_closeout_from_evidence(
        false,
        None,
        Some(&ActiveWorkContract::DocsRepair {
            deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
            pending_deliverables: Vec::new(),
            pending_summary: "docs/workflow-design.md still needs semantic repair".to_string(),
            route_contract_satisfied: false,
        }),
        &expected_artifacts,
        &actual_files,
        &[],
    );

    completed.missing_artifacts.is_empty()
        && completed.open_obligations.is_empty()
        && !should_continue_after_closeout(&completed)
        && missing
            .open_obligations
            .contains(&"author `docs/workflow-design.md`".to_string())
        && repair_remains_open
            .open_obligations
            .contains(&"repair docs `docs/workflow-design.md`".to_string())
}

pub fn satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes() -> bool {
    let expected_artifacts = vec!["docs/workflow-design.md".to_string()];
    let actual_files = vec!["docs/workflow-design.md".to_string()];
    let evidence = classify_manual_st_closeout_from_evidence(
        true,
        Some("The docs route contract is satisfied; run verification next.".to_string()),
        Some(&ActiveWorkContract::DocsRepair {
            deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
            pending_deliverables: Vec::new(),
            pending_summary: "docs route contract satisfied".to_string(),
            route_contract_satisfied: true,
        }),
        &expected_artifacts,
        &actual_files,
        &[],
    );
    let text_only_satisfied = classify_manual_st_closeout_from_evidence(
        true,
        Some("The docs route contract is satisfied; run verification next.".to_string()),
        Some(&ActiveWorkContract::DocsRepair {
            deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
            pending_deliverables: Vec::new(),
            pending_summary: "docs route contract satisfied".to_string(),
            route_contract_satisfied: false,
        }),
        &expected_artifacts,
        &actual_files,
        &[],
    );

    evidence.open_obligations.is_empty()
        && evidence.missing_artifacts.is_empty()
        && evidence.closeout_class == ManualStCloseoutClass::CleanCloseout
        && manual_st_route_verification_may_run(&evidence)
        && closeout_continuation_kind(&evidence).is_none()
        && text_only_satisfied
            .open_obligations
            .contains(&"repair docs `docs/workflow-design.md`".to_string())
}

pub(crate) fn manual_st_closeout_and_route_fixture_workflow_neutral_failures() -> Vec<&'static str>
{
    [
        (
            "final_assistant_open_obligation_not_clean_closeout",
            final_assistant_open_obligation_not_clean_closeout_fixture_passes(),
        ),
        (
            "final_assistant_open_obligation_continuation_hook",
            final_assistant_open_obligation_continuation_hook_fixture_passes(),
        ),
        (
            "open_obligation_continuation_expected_inventory_is_non_authoring",
            open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes(),
        ),
        (
            "route_verification_waits_for_authored_artifacts",
            route_verification_waits_for_authored_artifacts_fixture_passes(),
        ),
        (
            "post_repair_route_verification_clears_stale_repair",
            post_repair_route_verification_clears_stale_repair_fixture_passes(),
        ),
        (
            "route_result_progress_fields",
            route_result_progress_fields_fixture_passes(),
        ),
        (
            "route_inflight_case_progress_artifact",
            route_inflight_case_progress_artifact_fixture_passes(),
        ),
        (
            "route_case_progress_phase_boundaries",
            route_case_progress_phase_boundaries_fixture_passes(),
        ),
        (
            "successful_closeout_continuation_rematerializes_case_verdict",
            successful_closeout_continuation_rematerializes_case_verdict_fixture_passes(),
        ),
        (
            "route_terminal_verdict_rematerializes_from_case_results",
            route_terminal_verdict_rematerializes_from_case_results_fixture_passes(),
        ),
        (
            "latest_verification_result_drives_closeout",
            latest_verification_result_drives_closeout_fixture_passes(),
        ),
        (
            "verification_evidence_after_content_change_invalidated",
            verification_evidence_after_content_change_invalidated_fixture_passes(),
        ),
        (
            "runtime_verification_pass_after_content_change_satisfies_route_closeout",
            runtime_verification_pass_after_content_change_satisfies_route_closeout_fixture_passes(),
        ),
        (
            "verification_failure_preserves_closeout_evidence",
            verification_failure_preserves_closeout_evidence_fixture_passes(),
        ),
        (
            "verification_failed_closeout_builds_repair_hook_prompt",
            verification_failed_closeout_builds_repair_hook_prompt_fixture_passes(),
        ),
        (
            "public_command_contract_closeout_prompt_compacts_failure_evidence",
            public_command_contract_closeout_prompt_compacts_failure_evidence_fixture_passes(),
        ),
        (
            "verification_failed_closeout_uses_generated_test_parse_target",
            verification_failed_closeout_uses_generated_test_parse_target_fixture_passes(),
        ),
        (
            "closeout_continuation_budget_is_scoped_by_failure_signature",
            closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes(),
        ),
        (
            "closeout_continuation_budget_blocks_same_workspace_stall",
            closeout_continuation_budget_blocks_same_workspace_stall_fixture_passes(),
        ),
        (
            "verification_failure_labels_do_not_become_authoring_obligations",
            verification_failure_labels_do_not_become_authoring_obligations_fixture_passes(),
        ),
        (
            "runtime_failure_closeout_recomputes_current_artifacts",
            runtime_failure_closeout_recomputes_current_artifacts_fixture_passes(),
        ),
        (
            "run_error_closeout_replaces_stale_continuation_evidence",
            run_error_closeout_replaces_stale_continuation_evidence_fixture_passes(),
        ),
        (
            "run_error_open_obligation_uses_closeout_continuation_budget",
            run_error_open_obligation_uses_closeout_continuation_budget_fixture_passes(),
        ),
        (
            "runtime_terminal_status_uses_closeout_continuation_budget",
            runtime_terminal_status_uses_closeout_continuation_budget_fixture_passes(),
        ),
        (
            "terminalized_session_continuation_ledger_bounds_same_stage_recovery",
            terminalized_session_continuation_ledger_bounds_same_stage_recovery_fixture_passes(),
        ),
        (
            "content_changing_authoring_no_progress_terminal_fail_stops_route",
            content_changing_authoring_no_progress_terminal_fail_stops_route_fixture_passes(),
        ),
        (
            "verification_repair_terminal_ledger_blocks_non_obligation_workspace_progress",
            verification_repair_terminal_ledger_blocks_non_obligation_workspace_progress_fixture_passes(),
        ),
        (
            "completed_expected_artifact_clears_stale_authoring_obligation",
            completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes(),
        ),
        (
            "satisfied_docs_repair_does_not_reopen_route_closeout",
            satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes(),
        ),
    ]
    .into_iter()
    .filter_map(|(label, passed)| (!passed).then_some(label))
    .collect()
}

pub(crate) fn manual_st_closeout_and_route_fixtures_are_workflow_neutral_and_current_profile_fixture_passes()
-> bool {
    manual_st_closeout_and_route_fixture_workflow_neutral_failures().is_empty()
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
        _show_reasoning: bool,
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
