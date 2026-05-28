use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

use crate::agent::prompt::{ArtifactTargetKind, classify_artifact_target};
use crate::session::{MessagePart, TodoItem, ToolCallStatus, Transcript};
use crate::tool::ToolName;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerificationRequirements {
    pub any: bool,
    pub unit: bool,
    pub integration: bool,
    pub rust_build: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerificationEvidence {
    pub any: bool,
    pub unit: bool,
    pub integration: bool,
    pub rust_build: bool,
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
        self.any || self.unit || self.integration || self.rust_build
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
        if self.rust_build && !evidence.rust_build {
            missing.push("Rust compile verification");
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
        let unit = contains_any(&lower, UNIT_TOKENS) || looks_like_python_test_runner(&lower);
        let integration =
            contains_any(&lower, INTEGRATION_TOKENS) || looks_like_integration_runner(&lower);
        let rust_build = contains_any(&lower, RUST_BUILD_TOKENS);
        let generic = unit || integration || rust_build || contains_any(&lower, GENERIC_TOKENS);
        self.any |= generic;
        self.unit |= unit;
        self.integration |= integration;
        self.rust_build |= rust_build;
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
    "pytest",
    "cargo test",
    "cargo check",
    "go test",
    "python -m py_compile",
    "python -m unittest",
    "unittest",
    "テストを実行",
    "テスト実行",
    "テストが通る",
    "テストを通す",
];
const UNIT_TOKENS: &[&str] = &[
    "unit test",
    "unit tests",
    "unittest",
    "python -m unittest",
    "単体テスト",
];
const INTEGRATION_TOKENS: &[&str] = &[
    "integration test",
    "integration tests",
    "e2e",
    "end-to-end",
    "end to end",
    "統合テスト",
    "結合テスト",
];
const RUST_BUILD_TOKENS: &[&str] = &["cargo test", "cargo check", "cargo build"];
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
    transcript: &Transcript,
    start_index: usize,
    freshness_targets: &[Utf8PathBuf],
) -> VerificationEvidence {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return VerificationEvidence::default();
    };
    verification_progress_from_range_with_freshness(transcript, latest_user + 1, freshness_targets)
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
    transcript: &Transcript,
    start_index: usize,
    todos: &[TodoItem],
) -> Vec<Utf8PathBuf> {
    let mut targets = verification_freshness_targets_from_todos(todos)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return targets.into_iter().collect();
    };
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::DiffSummary(value) = &part.payload {
                for target in extract_diff_summary_targets_with_workspace(
                    &value.summary,
                    &transcript.session.cwd,
                ) {
                    let target = Utf8PathBuf::from(target);
                    if classify_artifact_target(target.as_str())
                        != ArtifactTargetKind::Documentation
                        && !is_noise_only_verification_target(target.as_str())
                    {
                        targets.insert(target);
                    }
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

pub(crate) fn latest_verification_repair_cycle(
    transcript: &Transcript,
) -> Option<VerificationRepairCycle> {
    let mut tool_calls = HashMap::new();
    for message in &transcript.messages {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                let read_span = verification_repair_read_span_from_tool_call(
                    value.tool_name,
                    &value.arguments_json,
                    &transcript.session.cwd,
                );
                tool_calls.insert(value.tool_call_id, (value.tool_name, command, read_span));
            }
        }
    }

    let mut failure_ordinal = 0usize;
    let mut cycle = None;
    for message in &transcript.messages {
        for part in &message.parts {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.status != ToolCallStatus::Completed {
                continue;
            }
            let Some((tool_name, command, read_span)) = tool_calls.get(&value.tool_call_id) else {
                continue;
            };

            if *tool_name == ToolName::Shell
                && looks_like_verification_command(command.as_deref(), &value.title)
            {
                if looks_like_verification_failure(command.as_deref(), &value.title, &value.summary)
                {
                    failure_ordinal += 1;
                    cycle = Some(VerificationRepairCycle {
                        failure_ordinal,
                        failed_command: command.clone().unwrap_or_default(),
                        repair_recorded: false,
                        post_failure_read_attempt_count: 0,
                        post_failure_read_targets: Vec::new(),
                        post_failure_read_spans: Vec::new(),
                    });
                } else if verification_output_looks_successful(&value.title, &value.summary) {
                    cycle = None;
                }
                continue;
            }

            let Some(current_cycle) = cycle.as_mut() else {
                continue;
            };
            if matches!(tool_name, ToolName::Write | ToolName::ApplyPatch)
                && verification_repair_result_counts_as_progress(value)
            {
                current_cycle.repair_recorded = true;
                continue;
            }
            if current_cycle.repair_recorded {
                continue;
            }
            if let Some(span) = read_span
                .as_ref()
                .filter(|_| verification_repair_read_result_counts_as_context(value))
            {
                current_cycle.post_failure_read_attempt_count += 1;
                insert_unique_target(
                    &mut current_cycle.post_failure_read_targets,
                    span.target.clone(),
                );
                current_cycle.post_failure_read_spans.push(span.clone());
            }
        }
    }

    cycle
}

pub(crate) fn latest_failed_verification_preceding_repair_targets(
    transcript: &Transcript,
) -> Vec<Utf8PathBuf> {
    let mut tool_calls = HashMap::new();
    for message in &transcript.messages {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                tool_calls.insert(value.tool_call_id, (value.tool_name, command));
            }
        }
    }

    let mut last_repair_targets: Vec<Utf8PathBuf> = Vec::new();
    let mut latest_failed_after_targets: Vec<Utf8PathBuf> = Vec::new();

    for message in &transcript.messages {
        for part in &message.parts {
            match &part.payload {
                MessagePart::DiffSummary(value) => {
                    let changed_targets = extract_diff_summary_targets_with_workspace(
                        &value.summary,
                        &transcript.session.cwd,
                    )
                    .into_iter()
                    .map(Utf8PathBuf::from)
                    .filter(|target| !is_noise_only_verification_target(target.as_str()))
                    .collect::<Vec<_>>();
                    if !changed_targets.is_empty() {
                        last_repair_targets = changed_targets;
                    }
                }
                MessagePart::ToolResult(value) => {
                    if value.status != ToolCallStatus::Completed {
                        continue;
                    }
                    let Some((tool_name, command)) = tool_calls.get(&value.tool_call_id) else {
                        continue;
                    };
                    if *tool_name != ToolName::Shell
                        || !looks_like_verification_command(command.as_deref(), &value.title)
                    {
                        continue;
                    }
                    if looks_like_verification_failure(
                        command.as_deref(),
                        &value.title,
                        &value.summary,
                    ) {
                        latest_failed_after_targets = last_repair_targets.clone();
                    } else if verification_output_looks_successful(&value.title, &value.summary) {
                        latest_failed_after_targets.clear();
                        last_repair_targets.clear();
                    }
                }
                _ => {}
            }
        }
    }

    latest_failed_after_targets
}

pub(crate) fn looks_like_verification_command(command: Option<&str>, title: &str) -> bool {
    let command = command
        .and_then(normalize_verification_command)
        .unwrap_or_else(|| command.unwrap_or_default().to_ascii_lowercase());
    let title = title.to_ascii_lowercase();
    command.contains("python -m unittest")
        || command.contains("python -m py_compile")
        || looks_like_python_test_runner(&command)
        || command.contains("pytest")
        || command.contains("cargo test")
        || command.contains("cargo check")
        || title.contains("python -m unittest")
        || title.contains("python -m py_compile")
        || title.contains("run tests")
        || looks_like_python_test_runner(&title)
        || title.contains("pytest")
        || title.contains("cargo test")
        || title.contains("cargo check")
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
    requirements.rust_build |= flags.rust_build;
}

fn requirement_flags_from_text(text: &str) -> VerificationRequirements {
    let lower = text.to_lowercase();
    let unit = contains_any(&lower, UNIT_TOKENS) || looks_like_python_test_runner(&lower);
    let integration = integration_verification_requirement_from_text(text, &lower);
    let rust_build =
        contains_any(&lower, RUST_BUILD_TOKENS) || implies_rust_project_verification(text, &lower);
    let generic = unit || integration || rust_build || contains_any(&lower, GENERIC_TOKENS);
    VerificationRequirements {
        any: generic,
        unit,
        integration,
        rust_build,
    }
}

fn contains_any(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lower.contains(needle))
}

fn looks_like_python_test_runner(lower: &str) -> bool {
    (lower.contains("python") || lower.starts_with("py "))
        && (lower.contains("test_")
            || lower.contains("_test.py")
            || lower.contains("/tests/")
            || lower.contains("\\tests\\"))
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

fn implies_rust_project_verification(text: &str, lower: &str) -> bool {
    if contains_explicit_file_target(text) {
        return false;
    }
    lower.starts_with("rust ")
        || (lower.contains("rust")
            && contains_any(
                lower,
                &[
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
                ],
            ))
}

fn contains_explicit_file_target(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        let candidate = token
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ','
                )
            })
            .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';' | '!' | '?'))
            .trim_start_matches(|ch: char| matches!(ch, '*' | '-' | '+'));
        !candidate.is_empty()
            && (candidate.contains('/') || candidate.contains('\\') || candidate.contains('.'))
    })
}

fn verification_evidence_from_range(
    transcript: &Transcript,
    message_start_index: usize,
) -> VerificationEvidence {
    let mut tool_calls = HashMap::new();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                tool_calls.insert(value.tool_call_id, (value.tool_name, command));
            }
        }
    }

    let mut evidence = VerificationEvidence::default();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            if let MessagePart::ToolResult(value) = &part.payload {
                if value.status != ToolCallStatus::Completed {
                    continue;
                }
                let Some((tool_name, command)) = tool_calls.get(&value.tool_call_id) else {
                    continue;
                };
                if *tool_name != ToolName::Shell {
                    continue;
                }
                if !looks_like_verification_command(command.as_deref(), &value.title) {
                    continue;
                }
                if !verification_output_looks_successful(&value.title, &value.summary) {
                    continue;
                }
                if let Some(command) = command {
                    evidence.record_from_text(command);
                }
                evidence.record_from_text(&value.title);
                evidence.record_from_text(&value.summary);
            }
        }
    }

    evidence
}

fn successful_verification_commands_from_range(
    transcript: &Transcript,
    message_start_index: usize,
) -> Vec<String> {
    let mut tool_calls = HashMap::new();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                tool_calls.insert(value.tool_call_id, (value.tool_name, command));
            }
        }
    }

    let mut commands = Vec::new();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            if let MessagePart::ToolResult(value) = &part.payload {
                if value.status != ToolCallStatus::Completed {
                    continue;
                }
                let Some((tool_name, command)) = tool_calls.get(&value.tool_call_id) else {
                    continue;
                };
                if *tool_name != ToolName::Shell {
                    continue;
                }
                if !looks_like_verification_command(command.as_deref(), &value.title) {
                    continue;
                }
                if !verification_output_looks_successful(&value.title, &value.summary) {
                    continue;
                }
                let Some(command) = command.as_deref() else {
                    continue;
                };
                if let Some(normalized) = normalize_verification_command(command) {
                    commands.push(normalized);
                }
            }
        }
    }
    dedupe_commands(commands)
}

#[derive(Debug, Default)]
struct VerificationProgress {
    evidence: VerificationEvidence,
    commands: Vec<String>,
}

fn verification_progress_from_range_with_freshness(
    transcript: &Transcript,
    message_start_index: usize,
    freshness_targets: &[Utf8PathBuf],
) -> VerificationProgress {
    let freshness_keys = verification_freshness_target_keys(freshness_targets);
    if freshness_keys.is_empty() {
        return VerificationProgress {
            evidence: verification_evidence_from_range(transcript, message_start_index),
            commands: successful_verification_commands_from_range(transcript, message_start_index),
        };
    }

    let mut tool_calls = HashMap::new();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                tool_calls.insert(value.tool_call_id, (value.tool_name, command));
            }
        }
    }

    let mut progress = VerificationProgress::default();
    for message in &transcript.messages[message_start_index..] {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolResult(value) => {
                    if value.status != ToolCallStatus::Completed {
                        continue;
                    }
                    let Some((tool_name, command)) = tool_calls.get(&value.tool_call_id) else {
                        continue;
                    };
                    if *tool_name != ToolName::Shell {
                        continue;
                    }
                    if !looks_like_verification_command(command.as_deref(), &value.title) {
                        continue;
                    }
                    if !verification_output_looks_successful(&value.title, &value.summary) {
                        continue;
                    }
                    if let Some(command) = command {
                        progress.evidence.record_from_text(command);
                        if let Some(normalized) = normalize_verification_command(command) {
                            progress.commands.push(normalized);
                        }
                    }
                    progress.evidence.record_from_text(&value.title);
                    progress.evidence.record_from_text(&value.summary);
                }
                MessagePart::DiffSummary(value) => {
                    let changed_targets = extract_diff_summary_targets_with_workspace(
                        &value.summary,
                        &transcript.session.cwd,
                    );
                    if changed_targets
                        .iter()
                        .any(|target| freshness_keys.contains(target))
                    {
                        progress = VerificationProgress::default();
                    }
                }
                _ => {}
            }
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

fn verification_repair_result_counts_as_progress(value: &crate::session::ToolResultPart) -> bool {
    !verification_repair_result_is_nonprogress(value)
}

fn verification_repair_read_result_counts_as_context(
    value: &crate::session::ToolResultPart,
) -> bool {
    !verification_repair_result_is_nonprogress(value)
}

fn verification_repair_result_is_nonprogress(value: &crate::session::ToolResultPart) -> bool {
    value.success == Some(false)
        || matches!(
            value.progress_effect,
            crate::protocol::ToolProgressEffect::NoProgress
                | crate::protocol::ToolProgressEffect::Blocked
                | crate::protocol::ToolProgressEffect::VerificationFailed
        )
        || verification_output_is_nonexecution(&value.title, &value.summary)
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
                    | ".next"
                    | "playwright-report"
                    | "test-results"
            )
        })
}

fn extract_diff_summary_targets_with_workspace(
    text: &str,
    workspace_root: &Utf8Path,
) -> Vec<String> {
    let mut targets = BTreeSet::new();
    for raw in text.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                ',' | ':' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
            )
    }) {
        let candidate = raw.trim_matches(|ch: char| matches!(ch, '`' | '.' | '!' | '?'));
        if candidate.is_empty() {
            continue;
        }
        if !(candidate.contains('/')
            || candidate.contains('\\')
            || Utf8Path::new(candidate).extension().is_some())
        {
            continue;
        }
        if let Some(target) = normalize_verification_target_path(candidate, workspace_root) {
            targets.insert(normalize_verification_target_key(target.as_str()));
        }
    }
    targets.into_iter().collect()
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
    const PREFIXES: &[&str] = &[
        "python -x utf8 -m unittest",
        "python -m py_compile",
        "python -m unittest",
        "cargo test",
        "cargo check",
        "cargo build",
        "go test",
        "pytest",
    ];

    let lower = line.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for prefix in PREFIXES {
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
    let collapsed = normalize_closed_network_verification_command(&collapsed);
    let collapsed = verification_command_suffix_boundary(&collapsed)
        .map(|index| collapsed[..index].trim().to_string())
        .unwrap_or(collapsed);
    let collapsed = collapsed
        .trim_end_matches(|ch: char| matches!(ch, '.' | '．' | ':'))
        .trim();
    normalize_python_unittest_command(collapsed)
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

fn normalize_python_unittest_command(text: &str) -> String {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() || !tokens[0].eq_ignore_ascii_case("python") {
        return text.to_string();
    }

    let mut index = 1usize;
    while index + 1 < tokens.len()
        && (tokens[index].eq_ignore_ascii_case("-x") || tokens[index].eq_ignore_ascii_case("-X"))
        && tokens[index + 1].eq_ignore_ascii_case("utf8")
    {
        index += 2;
    }
    if index + 1 < tokens.len()
        && tokens[index].eq_ignore_ascii_case("-m")
        && tokens[index + 1].eq_ignore_ascii_case("unittest")
    {
        let mut canonical = vec![
            "python".to_string(),
            "-m".to_string(),
            "unittest".to_string(),
        ];
        canonical.extend(tokens[index + 2..].iter().map(|token| token.to_string()));
        return canonical.join(" ");
    }

    text.to_string()
}

fn looks_like_explicit_verification_command(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.starts_with("python -m unittest")
        || lower.starts_with("python -x utf8 -m unittest")
        || lower.starts_with("python -m py_compile")
        || lower.starts_with("pytest")
        || lower.starts_with("cargo test")
        || lower.starts_with("cargo check")
        || lower.starts_with("cargo build")
        || lower.starts_with("go test")
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
        "python" | "python3" | "py" => {
            let mut index = 1usize;
            while index < tokens.len() {
                let token = tokens[index].to_ascii_lowercase();
                if token == "-x" && index + 1 < tokens.len() {
                    index += 2;
                    continue;
                }
                if token == "-m" {
                    return false;
                }
                break;
            }
            tokens[index..]
                .iter()
                .any(|token| token.to_ascii_lowercase().ends_with(".py"))
        }
        "node" => tokens
            .iter()
            .skip(1)
            .any(|token| token.to_ascii_lowercase().ends_with(".js")),
        _ => false,
    }
}

fn normalize_closed_network_verification_command(text: &str) -> String {
    let collapsed = collapse_whitespace(text.trim());
    if collapsed.to_ascii_lowercase().starts_with("uv run pytest") {
        return collapsed["uv run ".len()..].to_string();
    }
    collapsed
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

fn latest_user_index(transcript: &Transcript, start_index: usize) -> Option<usize> {
    transcript.messages[start_index..]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, message)| {
            matches!(message.record.role, crate::session::MessageRole::User)
                .then_some(start_index + offset)
        })
}
