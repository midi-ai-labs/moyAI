use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use crate::agent::language_evidence::{
    LANGUAGE_VERIFICATION_COMMAND_PREFIXES, language_build_check_verification_evidence,
    language_test_runner_evidence, language_verification_command_evidence,
    looks_like_language_direct_shell_verification_command,
    looks_like_language_explicit_verification_command, normalize_language_verification_command,
};
use crate::agent::prompt::{ArtifactTargetKind, classify_artifact_target};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemPayload, ToolLifecycleStatus, TurnId,
    VerificationRunStatus, canonical_tool_call_arguments,
};
use crate::session::{MessageRole, SessionId, TodoItem};
use crate::tool::ToolName;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerificationRequirements {
    pub any: bool,
    pub unit: bool,
    pub integration: bool,
    pub build_check: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerificationEvidence {
    pub any: bool,
    pub unit: bool,
    pub integration: bool,
    pub build_check: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct VerificationRepairCycle {
    pub failure_ordinal: usize,
    pub failed_command: String,
    pub repair_recorded: bool,
    pub post_failure_read_attempt_count: usize,
    pub post_failure_read_targets: Vec<Utf8PathBuf>,
    pub post_failure_read_spans: Vec<VerificationRepairReadSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerificationRepairReadSpan {
    pub target: Utf8PathBuf,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

impl VerificationRequirements {
    pub(crate) fn is_required(self) -> bool {
        self.any || self.unit || self.integration || self.build_check
    }

    pub(crate) fn is_satisfied_by(self, evidence: VerificationEvidence) -> bool {
        self.missing_labels(evidence).is_empty()
    }

    pub(crate) fn missing_labels(self, evidence: VerificationEvidence) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.unit && !evidence.unit {
            missing.push("unit test");
        }
        if self.integration && !evidence.integration {
            missing.push("integration test");
        }
        if self.build_check && !evidence.build_check {
            missing.push("build/check verification");
        }
        if missing.is_empty() && self.any && !evidence.any {
            missing.push("verification");
        }
        missing
    }
}

impl VerificationEvidence {
    fn record_from_text(&mut self, text: &str) {
        let lower = text.to_lowercase();
        let unit = contains_any(&lower, UNIT_TOKENS) || language_test_runner_evidence(&lower);
        let integration =
            contains_any(&lower, INTEGRATION_TOKENS) || looks_like_integration_runner(&lower);
        let build_check = language_build_check_verification_evidence(&lower)
            || contains_any(&lower, BUILD_CHECK_TOKENS);
        let generic = unit
            || integration
            || build_check
            || language_verification_command_evidence(&lower)
            || contains_any(&lower, GENERIC_TOKENS);
        self.any |= generic;
        self.unit |= unit;
        self.integration |= integration;
        self.build_check |= build_check;
    }
}

const GENERIC_TOKENS: &[&str] = &[
    "run tests",
    "run the tests",
    "run test suite",
    "run the test suite",
    "tests pass",
    "all tests pass",
    "ensure tests pass",
    "テストを実行",
    "テスト実行",
    "テストが通る",
    "テストを通す",
];
const UNIT_TOKENS: &[&str] = &["unit test", "unit tests", "単体テスト"];
const INTEGRATION_TOKENS: &[&str] = &[
    "integration test",
    "integration tests",
    "e2e",
    "end-to-end",
    "end to end",
    "統合テスト",
    "結合テスト",
];
const BUILD_CHECK_TOKENS: &[&str] = &[
    "build check",
    "build verification",
    "compile check",
    "compile verification",
    "rust build",
    "rust compile",
    "rust check",
];
const VERIFICATION_FAILURE_TOKENS: &[&str] = &[
    "assertion failed",
    "can't open file",
    "could not compile",
    "command timed out",
    "error:",
    "error[",
    "failed (",
    "failures:",
    "importerror",
    "modulenotfounderror",
    "no matching package named",
    "no such file or directory",
    "no tests ran",
    "panicked at",
    "test result: failed",
    "traceback",
];
const VERIFICATION_NON_EXECUTION_TOKENS: &[&str] = &[
    "verification rerun blocked until repair",
    "verification shell blocked until todo phase transition",
    "verification shell focus required",
];

pub(crate) fn verification_requirements(
    latest_user_text: Option<&str>,
    todos: &[TodoItem],
) -> VerificationRequirements {
    let _ = todos;
    let mut requirements = VerificationRequirements::default();
    if let Some(text) = latest_user_text {
        apply_requirement_text(&mut requirements, text);
    }
    requirements
}

pub(crate) fn verification_evidence_after_latest_user_with_freshness(
    history_items: &[HistoryItem],
    start_index: usize,
    freshness_targets: &[Utf8PathBuf],
) -> VerificationEvidence {
    let canonical_history_items = canonical_history_items_for_verification(history_items);
    let history_items = canonical_history_items.as_ref();
    let start_index = start_index.min(history_items.len());
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return VerificationEvidence::default();
    };
    verification_progress_from_history_items_with_freshness(
        history_items,
        latest_user + 1,
        freshness_targets,
    )
    .evidence
}

pub(crate) fn explicit_verification_commands_from_text(text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for candidate in verification_command_candidates(trimmed) {
            if let Some(command) = normalize_verification_command(&candidate) {
                commands.push(command);
            }
        }
    }
    dedupe_commands(commands)
}

fn verification_freshness_targets_from_todos(todos: &[TodoItem]) -> Vec<Utf8PathBuf> {
    let _ = todos;
    Vec::new()
}

pub(crate) fn verification_freshness_targets_after_latest_user(
    history_items: &[HistoryItem],
    start_index: usize,
    todos: &[TodoItem],
) -> Vec<Utf8PathBuf> {
    let mut targets = verification_freshness_targets_from_todos(todos)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let canonical_history_items = canonical_history_items_for_verification(history_items);
    let history_items = canonical_history_items.as_ref();
    let start_index = start_index.min(history_items.len());
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return targets.into_iter().collect();
    };
    for item in history_items.iter().skip(latest_user + 1) {
        if let HistoryItemPayload::FileChange { changes, .. } = &item.payload {
            for target in changes
                .iter()
                .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
            {
                if classify_artifact_target(target.as_str()) != ArtifactTargetKind::Documentation
                    && !is_noise_only_verification_target(target.as_str())
                {
                    targets.insert(Utf8PathBuf::from(normalize_verification_target_key(
                        target.as_str(),
                    )));
                }
            }
        }
    }
    targets.into_iter().collect()
}

pub(crate) fn verification_command_identity_key(text: &str) -> Option<String> {
    verification_command_satisfaction_keys(text)
        .into_iter()
        .next()
}

pub(crate) fn canonical_verification_command_identity_key(text: &str) -> Option<String> {
    verification_command_identity_key(text).or_else(|| direct_shell_command_identity_key(text))
}

pub(crate) fn verification_command_satisfaction_keys(text: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Some(command) = normalize_verification_command(text) {
        keys.insert(collapse_whitespace(&command).to_ascii_lowercase());
    }
    for candidate in extract_inline_verification_command_candidates(text) {
        let normalized = normalize_command_candidate(&candidate);
        if looks_like_explicit_verification_command(&normalized) {
            keys.insert(collapse_whitespace(&normalized).to_ascii_lowercase());
        }
    }
    keys
}

pub(crate) fn latest_verification_repair_cycle_from_history_items(
    history_items: &[HistoryItem],
    start_index: usize,
    workspace_root: &Utf8Path,
) -> Option<VerificationRepairCycle> {
    let canonical_history_items = canonical_history_items_for_verification(history_items);
    let history_items = canonical_history_items.as_ref();
    let start_index = start_index.min(history_items.len());
    let mut tool_calls = HashMap::new();
    let mut failure_ordinal = 0usize;
    let mut cycle = None;

    for item in history_items.iter().skip(start_index) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                let arguments_json =
                    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
                let command = if *tool == ToolName::Shell {
                    extract_shell_command(&arguments_json)
                } else {
                    None
                };
                let read_span = verification_repair_read_span_from_tool_call(
                    *tool,
                    &arguments_json,
                    workspace_root,
                );
                tool_calls.insert(call_id.to_string(), (*tool, command, read_span));
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                title,
                output_text,
                success,
                progress_effect,
                verification_run,
                ..
            } if *status == ToolLifecycleStatus::Completed => {
                let Some((tool_name, command, read_span)) = tool_calls.get(&call_id.to_string())
                else {
                    continue;
                };
                let verification_command = verification_run
                    .as_ref()
                    .map(|run| run.command.as_str())
                    .or(command.as_deref());
                let typed_verification_run = verification_run
                    .as_ref()
                    .is_some_and(|run| run.status != VerificationRunStatus::NotVerification);
                let shell_verification_output = *tool_name == ToolName::Shell
                    && (typed_verification_run
                        || looks_like_verification_command(verification_command, title));
                if shell_verification_output {
                    if verification_run.as_ref().is_some_and(|run| {
                        matches!(
                            run.status,
                            VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
                        )
                    }) || looks_like_verification_failure(
                        verification_command,
                        title,
                        output_text,
                    ) {
                        failure_ordinal += 1;
                        cycle = Some(VerificationRepairCycle {
                            failure_ordinal,
                            failed_command: verification_command.unwrap_or_default().to_string(),
                            repair_recorded: false,
                            post_failure_read_attempt_count: 0,
                            post_failure_read_targets: Vec::new(),
                            post_failure_read_spans: Vec::new(),
                        });
                    } else if verification_run
                        .as_ref()
                        .is_some_and(|run| run.status == VerificationRunStatus::Passed)
                        || verification_output_looks_successful(title, output_text)
                    {
                        cycle = None;
                    }
                    continue;
                }

                let Some(current_cycle) = cycle.as_mut() else {
                    continue;
                };
                if matches!(tool_name, ToolName::Write | ToolName::ApplyPatch)
                    && history_tool_output_counts_as_repair_progress(
                        *success,
                        progress_effect.clone(),
                        title,
                        output_text,
                    )
                {
                    current_cycle.repair_recorded = true;
                    continue;
                }
                if current_cycle.repair_recorded {
                    continue;
                }
                if let Some(span) = read_span.as_ref().filter(|_| {
                    history_tool_output_counts_as_repair_context(
                        *success,
                        progress_effect.clone(),
                        title,
                        output_text,
                    )
                }) {
                    current_cycle.post_failure_read_attempt_count += 1;
                    insert_unique_target(
                        &mut current_cycle.post_failure_read_targets,
                        span.target.clone(),
                    );
                    current_cycle.post_failure_read_spans.push(span.clone());
                }
            }
            _ => {}
        }
    }

    cycle
}

fn canonical_history_items_for_verification(
    history_items: &[HistoryItem],
) -> Cow<'_, [HistoryItem]> {
    if history_items_in_canonical_order(history_items) {
        return Cow::Borrowed(history_items);
    }
    let mut sorted = history_items.to_vec();
    sorted.sort_by_key(history_item_order_key);
    Cow::Owned(sorted)
}

fn history_items_in_canonical_order(history_items: &[HistoryItem]) -> bool {
    history_items
        .windows(2)
        .all(|items| history_item_order_key(&items[0]) <= history_item_order_key(&items[1]))
}

fn history_item_order_key(item: &HistoryItem) -> (i64, i64) {
    (item.sequence_no, item.created_at_ms)
}

pub(crate) fn verification_history_sequence_primary_order_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let items = vec![
        verification_order_fixture_item(session_id, turn_id, 3, 1000, "third"),
        verification_order_fixture_item(session_id, turn_id, 1, 3000, "first"),
        verification_order_fixture_item(session_id, turn_id, 2, 2000, "second"),
    ];
    let ordered = canonical_history_items_for_verification(&items);
    let sequence_order = ordered
        .iter()
        .map(|item| item.sequence_no)
        .collect::<Vec<_>>();

    sequence_order == vec![1, 2, 3] && history_items_in_canonical_order(ordered.as_ref())
}

fn verification_order_fixture_item(
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
    created_at_ms: i64,
    label: &str,
) -> HistoryItem {
    HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms,
        payload: HistoryItemPayload::Message {
            message_id: None,
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: label.to_string(),
            }],
        },
    }
}

fn history_tool_output_counts_as_repair_progress(
    success: Option<bool>,
    progress_effect: crate::protocol::ToolProgressEffect,
    title: &str,
    output_text: &str,
) -> bool {
    !history_tool_output_is_repair_nonprogress(success, progress_effect, title, output_text)
}

fn history_tool_output_counts_as_repair_context(
    success: Option<bool>,
    progress_effect: crate::protocol::ToolProgressEffect,
    title: &str,
    output_text: &str,
) -> bool {
    !history_tool_output_is_repair_nonprogress(success, progress_effect, title, output_text)
}

fn history_tool_output_is_repair_nonprogress(
    success: Option<bool>,
    progress_effect: crate::protocol::ToolProgressEffect,
    title: &str,
    output_text: &str,
) -> bool {
    success == Some(false)
        || matches!(
            progress_effect,
            crate::protocol::ToolProgressEffect::NoProgress
                | crate::protocol::ToolProgressEffect::Blocked
                | crate::protocol::ToolProgressEffect::VerificationFailed
        )
        || verification_output_is_nonexecution(title, output_text)
}

pub(crate) fn latest_failed_verification_preceding_repair_targets_from_history_items(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Vec<Utf8PathBuf> {
    let canonical_history_items = canonical_history_items_for_verification(history_items);
    let history_items = canonical_history_items.as_ref();
    let start_index = start_index.min(history_items.len());
    let mut tool_calls = HashMap::new();
    let mut last_repair_targets: Vec<Utf8PathBuf> = Vec::new();
    let mut latest_failed_after_targets: Vec<Utf8PathBuf> = Vec::new();

    for item in history_items.iter().skip(start_index) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                let arguments_json =
                    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
                let command = if *tool == ToolName::Shell {
                    extract_shell_command(&arguments_json)
                } else {
                    None
                };
                tool_calls.insert(call_id.to_string(), (*tool, command));
            }
            HistoryItemPayload::FileChange { changes, .. } => {
                let changed_targets = changes
                    .iter()
                    .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
                    .filter(|target| !is_noise_only_verification_target(target.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                if !changed_targets.is_empty() {
                    last_repair_targets = changed_targets;
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                verification_run,
                ..
            } if *status == ToolLifecycleStatus::Completed => {
                let Some((tool_name, _command)) = tool_calls.get(&call_id.to_string()) else {
                    continue;
                };
                if *tool_name != ToolName::Shell {
                    continue;
                }
                let Some(verification_run) = verification_run else {
                    continue;
                };
                match verification_run.status {
                    VerificationRunStatus::Failed | VerificationRunStatus::TimedOut => {
                        latest_failed_after_targets = last_repair_targets.clone();
                    }
                    VerificationRunStatus::Passed => {
                        latest_failed_after_targets.clear();
                        last_repair_targets.clear();
                    }
                    VerificationRunStatus::NotVerification => {}
                }
            }
            _ => {}
        }
    }

    latest_failed_after_targets
}

pub(crate) fn looks_like_verification_command(command: Option<&str>, title: &str) -> bool {
    let command = command
        .and_then(normalize_verification_command)
        .unwrap_or_else(|| command.unwrap_or_default().to_ascii_lowercase());
    let title = title.to_ascii_lowercase();
    language_verification_command_evidence(&command)
        || language_verification_command_evidence(&title)
        || title.contains("run tests")
        || title.contains("integration test")
}

pub(crate) fn looks_like_verification_failure(
    command: Option<&str>,
    title: &str,
    summary: &str,
) -> bool {
    if verification_output_is_nonexecution(title, summary) {
        return false;
    }
    let lower_summary = summary.to_lowercase();
    let verification_like = looks_like_verification_command(command, title)
        || lower_summary.contains("ran ")
        || lower_summary.contains("test result:")
        || lower_summary.contains("fail:")
        || lower_summary.contains("error:");
    verification_like && verification_output_has_failure_markers(summary)
}

fn apply_requirement_text(requirements: &mut VerificationRequirements, text: &str) {
    let flags = requirement_flags_from_text(text);
    requirements.any |= flags.any;
    requirements.unit |= flags.unit;
    requirements.integration |= flags.integration;
    requirements.build_check |= flags.build_check;
}

fn requirement_flags_from_text(text: &str) -> VerificationRequirements {
    let lower = text.to_lowercase();
    let unit = contains_any(&lower, UNIT_TOKENS) || language_test_runner_evidence(&lower);
    let integration = integration_verification_requirement_from_text(text, &lower);
    let build_check = language_build_check_verification_evidence(&lower)
        || contains_any(&lower, BUILD_CHECK_TOKENS)
        || implies_project_build_check_verification(text, &lower);
    let generic = unit
        || integration
        || build_check
        || language_verification_command_evidence(&lower)
        || contains_any(&lower, GENERIC_TOKENS);
    VerificationRequirements {
        any: generic,
        unit,
        integration,
        build_check,
    }
}

fn contains_any(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lower.contains(needle))
}

fn looks_like_integration_runner(lower: &str) -> bool {
    lower.contains("integration")
        || lower.contains("testintegration")
        || lower.contains("test_integration")
}

fn integration_verification_requirement_from_text(text: &str, lower: &str) -> bool {
    let mentions_integration =
        contains_any(lower, INTEGRATION_TOKENS) || looks_like_integration_runner(lower);
    if !mentions_integration {
        return false;
    }

    let authoring_only_markers = [
        "add integration test",
        "add integration tests",
        "integration test を追加",
        "integration tests を追加",
        "統合テストを追加",
        "統合テストも追加",
        "結合テストを追加",
        "結合テストも追加",
    ];
    let execution_markers = [
        "run integration",
        "execute integration",
        "integration test before completion",
        "integration tests before completion",
        "integration test pass",
        "integration tests pass",
        "integration test passes",
        "integration tests passes",
        "integration test succeeded",
        "integration tests succeeded",
        "integration test を実行",
        "integration test を通",
        "integration test が成功",
        "integration tests を実行",
        "integration tests を通",
        "integration tests が成功",
        "統合テストを実行",
        "統合テストを通",
        "統合テストが成功",
        "結合テストを実行",
        "結合テストを通",
        "結合テストが成功",
    ];
    let mut saw_authoring_only_integration = false;
    let mut saw_execution_integration = false;
    for chunk in text.split(['\n', '.', '。', ';', '；']) {
        let chunk_lower = chunk.to_lowercase();
        if !(contains_any(&chunk_lower, INTEGRATION_TOKENS)
            || looks_like_integration_runner(&chunk_lower))
        {
            continue;
        }
        let chunk_authoring_only = authoring_only_markers
            .iter()
            .any(|marker| chunk_lower.contains(marker));
        let chunk_execution = execution_markers
            .iter()
            .any(|marker| chunk_lower.contains(marker))
            || contains_any(
                &chunk_lower,
                &[
                    "run",
                    "execute",
                    "pass",
                    "passes",
                    "succeed",
                    "succeeds",
                    "verify",
                    "verification",
                    "実行",
                    "確認",
                    "成功",
                    "通",
                    "再実行",
                ],
            );
        if chunk_execution {
            saw_execution_integration = true;
        } else if chunk_authoring_only {
            saw_authoring_only_integration = true;
        }
    }

    if saw_authoring_only_integration && !saw_execution_integration {
        return false;
    }

    saw_execution_integration || !saw_authoring_only_integration
}

fn implies_project_build_check_verification(text: &str, lower: &str) -> bool {
    if contains_explicit_file_target(text) {
        return false;
    }
    let project_markers = [
        "app",
        "application",
        "build",
        "cli",
        "crate",
        "create",
        "game",
        "implement",
        "library",
        "make",
        "project",
        "tool",
        "write",
    ];
    let build_check_languages = [
        "rust",
        "crate",
        "cargo",
        "javascript",
        "react.js",
        "next.js",
        "vue.js",
        "node.js",
        "go",
        "golang",
        "java",
        "kotlin",
        "dotnet",
        "c#",
        "typescript",
        "ts",
    ];
    build_check_languages.iter().any(|language| {
        lower.starts_with(&format!("{language} "))
            || (lower.contains(language) && contains_any(lower, &project_markers))
    })
}

fn contains_explicit_file_target(text: &str) -> bool {
    let tokens = text
        .split_whitespace()
        .map(normalize_target_classifier_token)
        .collect::<Vec<_>>();
    tokens.iter().enumerate().any(|(index, candidate)| {
        let following = tokens.get(index + 1).map(String::as_str);
        explicit_file_target_token(candidate.as_str(), following)
    })
}

fn normalize_target_classifier_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ','
            )
        })
        .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';' | '!' | '?'))
        .trim_start_matches(|ch: char| matches!(ch, '*' | '-' | '+'))
        .to_string()
}

fn explicit_file_target_token(candidate: &str, following: Option<&str>) -> bool {
    if candidate.is_empty() {
        return false;
    }
    if candidate.contains('/') || candidate.contains('\\') {
        return true;
    }
    if !target_token_contains_dot(candidate) {
        return false;
    }
    if candidate.starts_with('.') {
        return true;
    }
    !dotted_technology_token_in_project_context(candidate, following)
}

fn target_token_contains_dot(candidate: &str) -> bool {
    candidate.chars().any(|ch| ch == '.')
}

fn dotted_technology_token_in_project_context(candidate: &str, following: Option<&str>) -> bool {
    let lower = candidate.to_ascii_lowercase();
    let known_dotted_technology = matches!(
        lower.as_str(),
        "react.js" | "next.js" | "vue.js" | "node.js" | "three.js" | "p5.js" | "d3.js"
    );
    if !known_dotted_technology {
        return false;
    }
    following
        .map(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "app"
                    | "application"
                    | "project"
                    | "tool"
                    | "site"
                    | "website"
                    | "frontend"
                    | "backend"
                    | "library"
                    | "cli"
                    | "game"
            )
        })
        .unwrap_or(false)
}

#[derive(Debug, Default)]
struct VerificationProgress {
    evidence: VerificationEvidence,
    commands: Vec<String>,
}

fn verification_progress_from_history_items_with_freshness(
    history_items: &[HistoryItem],
    item_start_index: usize,
    freshness_targets: &[Utf8PathBuf],
) -> VerificationProgress {
    let freshness_keys = verification_freshness_target_keys(freshness_targets);
    let mut tool_calls = HashMap::new();
    let mut progress = VerificationProgress::default();
    let item_start_index = item_start_index.min(history_items.len());

    for item in history_items.iter().skip(item_start_index) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                let arguments_json =
                    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
                let command = if *tool == ToolName::Shell {
                    extract_shell_command(&arguments_json)
                } else {
                    None
                };
                tool_calls.insert(call_id.to_string(), (*tool, command));
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                verification_run,
                ..
            } if *status == ToolLifecycleStatus::Completed => {
                let Some((tool_name, _command)) = tool_calls.get(&call_id.to_string()) else {
                    continue;
                };
                if *tool_name != ToolName::Shell {
                    continue;
                }
                let Some(verification_run) = verification_run else {
                    continue;
                };
                if verification_run.status != VerificationRunStatus::Passed {
                    continue;
                }
                progress
                    .evidence
                    .record_from_text(&verification_run.command);
                progress
                    .evidence
                    .record_from_text(&verification_run.output_summary);
                if let Some(normalized) = normalize_verification_command(&verification_run.command)
                {
                    progress.commands.push(normalized);
                }
            }
            HistoryItemPayload::FileChange { changes, .. } if !freshness_keys.is_empty() => {
                let freshness_changed = changes
                    .iter()
                    .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
                    .map(|target| normalize_verification_target_key(target.as_str()))
                    .any(|target| freshness_keys.contains(&target));
                if freshness_changed {
                    progress = VerificationProgress::default();
                }
            }
            _ => {}
        }
    }

    progress.commands = dedupe_commands(progress.commands);
    progress
}

fn verification_repair_read_span_from_tool_call(
    tool_name: ToolName,
    arguments_json: &str,
    workspace_root: &Utf8Path,
) -> Option<VerificationRepairReadSpan> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    let raw = match tool_name {
        ToolName::Read | ToolName::List | ToolName::InspectDirectory | ToolName::DoclingConvert => {
            value.get("path").and_then(Value::as_str)
        }
        ToolName::Grep => value.get("path").and_then(Value::as_str),
        _ => None,
    }?;
    let target = normalize_verification_target_path(raw, workspace_root)?;
    Some(VerificationRepairReadSpan {
        target,
        offset: value
            .get("offset")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        limit: value
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    })
}

fn insert_unique_target(targets: &mut Vec<Utf8PathBuf>, candidate: Utf8PathBuf) {
    let key = normalize_verification_target_key(candidate.as_str());
    if targets
        .iter()
        .any(|existing| normalize_verification_target_key(existing.as_str()) == key)
    {
        return;
    }
    targets.push(candidate);
}

fn verification_freshness_target_keys(targets: &[Utf8PathBuf]) -> BTreeSet<String> {
    targets
        .iter()
        .map(|target| normalize_verification_target_key(target.as_str()))
        .collect()
}

fn normalize_verification_target_key(target: &str) -> String {
    target.replace('\\', "/").to_ascii_lowercase()
}

fn is_noise_only_verification_target(target: &str) -> bool {
    let lower = target.replace('\\', "/").to_ascii_lowercase();
    lower.ends_with(".pyc")
        || lower.ends_with(".pyo")
        || lower.split('/').any(|segment| {
            matches!(
                segment,
                ".git"
                    | ".venv"
                    | ".pytest_cache"
                    | ".ruff_cache"
                    | "__pycache__"
                    | "node_modules"
                    | "target"
                    | "build-artifacts"
                    | ".next"
                    | "playwright-report"
                    | "test-results"
            )
        })
}

fn normalize_verification_target_path(
    candidate: &str,
    workspace_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Utf8Path::new(trimmed);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    absolute
        .strip_prefix(workspace_root)
        .ok()
        .map(Utf8Path::to_path_buf)
}

fn verification_output_looks_successful(title: &str, summary: &str) -> bool {
    let lower = format!("{title}\n{summary}").to_lowercase();
    !contains_any(&lower, VERIFICATION_FAILURE_TOKENS)
        && !verification_output_is_nonexecution(title, summary)
}

fn verification_output_has_failure_markers(summary: &str) -> bool {
    let lower = summary.to_lowercase();
    if contains_any(&lower, VERIFICATION_FAILURE_TOKENS) {
        return true;
    }

    summary.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }

        let lower = trimmed.to_ascii_lowercase();
        trimmed.starts_with("FAIL: ")
            || trimmed.starts_with("ERROR: ")
            || lower.starts_with("fail: ")
            || lower.starts_with("error: ")
            || lower.starts_with("error[")
            || lower.starts_with("failed (")
            || lower.starts_with("traceback")
            || lower.ends_with("... fail")
            || lower.ends_with("... error")
    })
}

fn verification_output_is_nonexecution(title: &str, summary: &str) -> bool {
    contains_any(
        &format!("{title}\n{summary}").to_lowercase(),
        VERIFICATION_NON_EXECUTION_TOKENS,
    )
}

fn verification_command_candidates(line: &str) -> Vec<String> {
    let mut candidates = split_backtick_spans(line);
    if candidates.is_empty() {
        candidates.extend(extract_inline_verification_commands(line));
    }
    if candidates.is_empty() {
        candidates.push(line.to_string());
    }
    candidates
}

fn split_backtick_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for ch in text.chars() {
        if ch == '`' {
            if inside && !current.trim().is_empty() {
                spans.push(current.trim().to_string());
            }
            current.clear();
            inside = !inside;
            continue;
        }
        if inside {
            current.push(ch);
        }
    }
    spans
}

fn normalize_verification_command(text: &str) -> Option<String> {
    let normalized = normalize_command_candidate(text);
    if looks_like_explicit_verification_command(&normalized) {
        return Some(normalized);
    }
    extract_inline_verification_command_candidates(text)
        .into_iter()
        .map(|candidate| normalize_command_candidate(&candidate))
        .find(|candidate| looks_like_explicit_verification_command(candidate))
}

fn extract_inline_verification_commands(line: &str) -> Vec<String> {
    dedupe_commands(extract_inline_verification_command_candidates(line))
}

fn extract_inline_verification_command_candidates(line: &str) -> Vec<String> {
    let lower = line.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for prefix in LANGUAGE_VERIFICATION_COMMAND_PREFIXES {
        let mut search_from = 0usize;
        while let Some(found) = lower[search_from..].find(prefix) {
            let start = search_from + found;
            let remainder = &line[start..];
            let end = inline_command_end_index(remainder);
            candidates.push(remainder[..end].trim().to_string());
            search_from = start + prefix.len();
        }
    }
    candidates
}

fn inline_command_end_index(text: &str) -> usize {
    verification_command_suffix_boundary(text).unwrap_or(text.len())
}

fn normalize_command_candidate(text: &str) -> String {
    let trimmed = text
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'))
        .trim_start_matches(|ch: char| matches!(ch, '-' | '*' | '+' | '•'))
        .trim();
    let collapsed = collapse_whitespace(trimmed);
    let collapsed = normalize_language_verification_command(&collapsed);
    let collapsed = verification_command_suffix_boundary(&collapsed)
        .map(|index| collapsed[..index].trim().to_string())
        .unwrap_or(collapsed);
    let collapsed = collapsed
        .trim_end_matches(|ch: char| matches!(ch, '.' | '．' | ':'))
        .trim();
    collapsed.to_string()
}

fn verification_command_suffix_boundary(text: &str) -> Option<usize> {
    const DELIMITERS: &[&str] = &[
        " succeeds",
        " succeed",
        " succeeded",
        " passes",
        " pass",
        " passed",
        " exits with",
        " exit with",
        " returns with",
        " return with",
        " completes with",
        " complete with",
        " should pass",
        " should succeed",
        " should ",
        " still ",
        " and ",
        " then ",
        " の",
        " が",
        " は",
        " を",
        " に",
        " で",
        " と",
        " して",
        " し",
        "が成功",
        "が通",
        "を実行",
        "を再実行",
        "を確認",
        "を通",
        "してください",
        "して下さい",
        ",",
        "，",
        "、",
        "。",
        ";",
        "；",
    ];

    DELIMITERS
        .iter()
        .filter_map(|delimiter| text.find(delimiter))
        .min()
}

fn looks_like_explicit_verification_command(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    looks_like_language_explicit_verification_command(&lower)
        || looks_like_direct_shell_verification_command(&lower)
}

fn direct_shell_command_identity_key(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }
    let normalized = normalize_command_candidate(trimmed);
    if !looks_like_direct_shell_verification_command(&normalized) {
        return None;
    }
    Some(collapse_whitespace(&normalized).to_ascii_lowercase())
}

fn looks_like_direct_shell_verification_command(text: &str) -> bool {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    let Some(program) = tokens.first().map(|token| token.to_ascii_lowercase()) else {
        return false;
    };
    match program.as_str() {
        "python" | "python3" | "py" | "node" => {
            looks_like_language_direct_shell_verification_command(text)
        }
        _ => looks_like_custom_verification_program(&program),
    }
}

fn looks_like_custom_verification_program(program: &str) -> bool {
    let leaf = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_start_matches("./")
        .trim_end_matches(".exe");
    leaf == "verify"
        || leaf == "check"
        || leaf == "test"
        || leaf.starts_with("verify-")
        || leaf.ends_with("-verify")
        || leaf.starts_with("check-")
        || leaf.ends_with("-check")
        || leaf.starts_with("test-")
        || leaf.ends_with("-test")
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn dedupe_commands(commands: Vec<String>) -> Vec<String> {
    let mut seen = HashMap::new();
    let mut deduped = Vec::new();
    for command in commands {
        let key = canonical_verification_command_identity_key(&command)
            .unwrap_or_else(|| collapse_whitespace(&command).to_ascii_lowercase());
        if seen.insert(key, ()).is_none() {
            deduped.push(command);
        }
    }
    deduped
}

fn extract_shell_command(arguments_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    value
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn latest_user_history_index(history_items: &[HistoryItem], start_index: usize) -> Option<usize> {
    history_items[start_index..]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, item)| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: crate::session::MessageRole::User,
                        ..
                    }
            )
            .then_some(start_index + offset)
        })
}

pub(crate) fn verification_requirements_use_generic_build_check_fixture_passes() -> bool {
    let rust_project = requirement_flags_from_text("Create a Rust CLI project.");
    let go_project = requirement_flags_from_text("Create a Go CLI project.");
    let explicit_cargo = requirement_flags_from_text("Run `cargo check` before completion.");
    let explicit_py_compile =
        requirement_flags_from_text("Run `python -m py_compile src/tool.py` before completion.");
    let explicit_tests = requirement_flags_from_text("Run `cargo test` before completion.");
    let mut cargo_check_evidence = VerificationEvidence::default();
    cargo_check_evidence.record_from_text("cargo check");
    let mut py_compile_evidence = VerificationEvidence::default();
    py_compile_evidence.record_from_text("python -m py_compile src/tool.py");
    rust_project.build_check
        && go_project.build_check
        && explicit_cargo.build_check
        && explicit_py_compile.build_check
        && explicit_tests.unit
        && !explicit_tests.build_check
        && cargo_check_evidence.build_check
        && !cargo_check_evidence.unit
        && py_compile_evidence.build_check
        && !py_compile_evidence.unit
        && VerificationRequirements {
            any: true,
            unit: false,
            integration: false,
            build_check: true,
        }
        .is_satisfied_by(cargo_check_evidence)
}

pub(crate) fn verification_dotted_technology_tokens_are_not_file_targets_fixture_passes() -> bool {
    let react_project = requirement_flags_from_text("Create a React.js app.");
    let next_project = requirement_flags_from_text("Create a Next.js project.");
    let explicit_source_file = requirement_flags_from_text("Create src/workflow.rs.");
    let explicit_markdown_file = requirement_flags_from_text("Update README.md.");

    react_project.build_check
        && next_project.build_check
        && !explicit_source_file.build_check
        && !explicit_markdown_file.build_check
        && !contains_explicit_file_target("Create a React.js app.")
        && !contains_explicit_file_target("Create a Next.js project.")
        && contains_explicit_file_target("Create src/workflow.rs.")
        && contains_explicit_file_target("Update README.md.")
}

pub(crate) fn verification_repair_cycle_uses_canonical_history_order_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let failed_check_call_id = crate::session::ToolCallId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let repair_call_id = crate::session::ToolCallId::new();

    let failed_check_call = verification_fixture_tool_call_item(
        session_id,
        turn_id,
        2,
        failed_check_call_id,
        ToolName::Shell,
        serde_json::json!({"command":"cargo test"}),
    );
    let failed_check_output = verification_fixture_tool_output_item(
        session_id,
        turn_id,
        3,
        failed_check_call_id,
        "cargo test failed",
        "test result: FAILED. assertion failed",
        Some(false),
        crate::protocol::ToolProgressEffect::VerificationFailed,
        Some(crate::protocol::VerificationRunResult {
            command: "cargo test".to_string(),
            status: VerificationRunStatus::Failed,
            exit_code: Some(101),
            timed_out: false,
            output_summary: "assertion failed".to_string(),
            failure_cluster: None,
            satisfies_command_identities: Vec::new(),
            artifact_refs: Vec::new(),
            requirement_refs: Vec::new(),
        }),
    );
    let read_call = verification_fixture_tool_call_item(
        session_id,
        turn_id,
        4,
        read_call_id,
        ToolName::Read,
        serde_json::json!({"path":"src/lib.rs"}),
    );
    let read_output = verification_fixture_tool_output_item(
        session_id,
        turn_id,
        5,
        read_call_id,
        "Read src/lib.rs",
        "fn broken() {}",
        Some(true),
        crate::protocol::ToolProgressEffect::Unknown,
        None,
    );
    let repair_call = verification_fixture_tool_call_item(
        session_id,
        turn_id,
        6,
        repair_call_id,
        ToolName::ApplyPatch,
        serde_json::json!({"path":"src/lib.rs","patch_text":"*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-fn broken() {}\n+fn fixed() {}\n*** End Patch\n"}),
    );
    let repair_output = verification_fixture_tool_output_item(
        session_id,
        turn_id,
        7,
        repair_call_id,
        "Patch applied",
        "Updated src/lib.rs",
        Some(true),
        crate::protocol::ToolProgressEffect::MadeProgress,
        None,
    );

    let out_of_order_history = vec![
        repair_call,
        repair_output,
        failed_check_call,
        failed_check_output,
        read_call,
        read_output,
    ];
    latest_verification_repair_cycle_from_history_items(
        &out_of_order_history,
        0,
        Utf8Path::new("."),
    )
    .is_some_and(|cycle| {
        cycle.failed_command == "cargo test"
            && cycle.repair_recorded
            && cycle.post_failure_read_attempt_count == 1
            && cycle
                .post_failure_read_targets
                .iter()
                .any(|target| target.as_str() == "src/lib.rs")
    })
}

pub(crate) fn verification_repair_cycle_history_item_authority_fixture_passes() -> bool {
    verification_repair_cycle_uses_canonical_history_order_fixture_passes()
}

fn verification_fixture_tool_call_item(
    session_id: crate::session::SessionId,
    turn_id: crate::protocol::TurnId,
    sequence_no: i64,
    call_id: crate::session::ToolCallId,
    tool: ToolName,
    arguments: Value,
) -> HistoryItem {
    HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: sequence_no,
        payload: HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments: arguments.clone(),
            model_arguments: Value::Null,
            effective_arguments: arguments,
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![tool],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    }
}

fn verification_fixture_tool_output_item(
    session_id: crate::session::SessionId,
    turn_id: crate::protocol::TurnId,
    sequence_no: i64,
    call_id: crate::session::ToolCallId,
    title: &str,
    output_text: &str,
    success: Option<bool>,
    progress_effect: crate::protocol::ToolProgressEffect,
    verification_run: Option<crate::protocol::VerificationRunResult>,
) -> HistoryItem {
    HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: sequence_no,
        payload: HistoryItemPayload::ToolOutput {
            call_id,
            status: ToolLifecycleStatus::Completed,
            title: title.to_string(),
            output_text: output_text.to_string(),
            metadata: Value::Null,
            success,
            progress_effect,
            blocked_action: None,
            result_hash: Some(format!("fixture-{sequence_no}")),
            verification_run,
        },
    }
}
