use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};

use crate::agent::language_evidence::{
    ArtifactRole, LanguageArtifactShapeContract, LanguageFamily,
    LanguageSourceArtifactShapeContract, classify_artifact_target,
    language_source_artifact_content_has_executable_shape,
    language_source_artifact_content_is_escaped_whole_file_string,
    language_source_artifact_forbidden_content_markers, language_source_artifact_shape_contract,
    language_source_line_has_code_shape, language_test_artifact_content_has_executable_shape,
    language_test_artifact_forbidden_content_markers, language_test_artifact_shape_contract,
};
use crate::edit::{PatchLine, PatchOperation, PatchParser};
use crate::protocol::OperationIntent;
use crate::tool::ToolResult;

fn test_artifact_shape_contract(target: &str) -> Option<LanguageArtifactShapeContract> {
    language_test_artifact_shape_contract(target)
}

fn source_artifact_shape_contract(target: &str) -> Option<LanguageSourceArtifactShapeContract> {
    language_source_artifact_shape_contract(target)
}

pub(crate) fn language_artifact_content_has_executable_shape(target: &str, content: &str) -> bool {
    if test_artifact_shape_contract(target).is_some() {
        return language_test_artifact_content_has_executable_shape(target, content);
    }
    language_source_artifact_content_has_executable_shape(target, content)
}

pub(crate) fn text_artifact_target_requires_readable_shape(target: &str) -> bool {
    let spec = classify_artifact_target(target);
    spec.language == LanguageFamily::Text && spec.role == ArtifactRole::Document
}

pub(crate) fn text_artifact_content_has_readable_shape(target: &str, content: &str) -> bool {
    if !text_artifact_target_requires_readable_shape(target) {
        return true;
    }
    !text_artifact_content_is_serialized_string_snapshot(content)
}

pub(crate) fn code_artifact_target_requires_effective_shape(target: &str) -> bool {
    let spec = classify_artifact_target(target);
    spec.language == LanguageFamily::Code
        && matches!(spec.role, ArtifactRole::Source | ArtifactRole::Test)
}

pub(crate) fn code_artifact_content_has_effective_shape(target: &str, content: &str) -> bool {
    if !code_artifact_target_requires_effective_shape(target) {
        return true;
    }
    let trimmed = content.trim();
    !trimmed.is_empty()
        && !code_artifact_content_is_serialized_string_snapshot(content)
        && !code_artifact_content_is_markdown_or_prose_payload(content)
        && code_artifact_content_has_code_shape(content)
}

pub(crate) fn text_artifact_content_is_serialized_string_snapshot(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let quote_wrapped = (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
    if !quote_wrapped {
        return false;
    }
    let real_newlines = trimmed.matches('\n').count();
    let escaped_newlines = trimmed.matches("\\n").count();
    if escaped_newlines > 0 || real_newlines > 0 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("\\n#")
        || lower.contains("\\n##")
        || lower.contains("\\n- ")
        || lower.contains("\\n* ")
        || lower.contains("\\n```")
        || lower.contains("\\n|")
}

fn code_artifact_content_is_serialized_string_snapshot(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let quote_wrapped = (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
    if !quote_wrapped {
        return false;
    }
    let real_newlines = trimmed.matches('\n').count();
    let escaped_newlines = trimmed.matches("\\n").count();
    escaped_newlines >= 2 && escaped_newlines > real_newlines.saturating_mul(3)
}

fn code_artifact_content_is_markdown_or_prose_payload(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed
        .lines()
        .any(|line| line.trim_start().starts_with("```"))
    {
        return true;
    }
    let has_code_shape = code_artifact_content_has_code_shape(content);
    let has_markdown_structure = trimmed.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("# ")
            || line.starts_with("## ")
            || line.starts_with("### ")
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("> ")
    });
    let prose_lines = trimmed
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty()
                && !code_artifact_line_is_comment(line)
                && !language_source_line_has_code_shape(LanguageFamily::Code, line)
                && code_artifact_line_looks_like_prose(line)
        })
        .count();
    (has_markdown_structure && !has_code_shape) || (prose_lines >= 2 && !has_code_shape)
}

fn code_artifact_content_has_code_shape(content: &str) -> bool {
    content
        .lines()
        .any(|line| language_source_line_has_code_shape(LanguageFamily::Code, line.trim()))
}

fn code_artifact_line_is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || (trimmed.starts_with('#') && !trimmed.starts_with("#!"))
        || trimmed.starts_with("--")
}

fn code_artifact_line_looks_like_prose(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.contains('。')
        || line.contains('、')
        || line.contains("です")
        || line.contains("ます")
        || lower.contains(" supports ")
        || lower.contains(" should ")
        || lower.contains(" must ")
        || lower.contains(" this ")
        || lower.contains(" that ")
        || lower.split_whitespace().count() >= 4
}

pub(crate) fn required_write_content_shape_violation_result(
    tool_name: &str,
    arguments: &Value,
    required_target: &str,
) -> Option<ToolResult> {
    required_write_content_shape_violation_result_with_requested_target(
        tool_name,
        arguments,
        required_target,
        None,
    )
}

pub(crate) fn required_write_content_shape_violation_result_with_requested_target(
    tool_name: &str,
    arguments: &Value,
    required_target: &str,
    requested_target: Option<&str>,
) -> Option<ToolResult> {
    if !matches!(tool_name, "write" | "apply_patch") {
        return None;
    }
    let content = arguments.get("content").and_then(Value::as_str)?;
    let content_shape_guidance = required_write_target_mismatch_content_shape_guidance(
        required_target,
        requested_target,
        content,
    );
    let content_shape_contract = artifact_content_shape_metadata(required_target);
    let forbidden_markers = detected_content_shape_forbidden_markers(required_target, content);
    let result_hash = crate::harness::artifact::hash_bytes(
        format!("required_write_content_shape_mismatch:{required_target}").as_bytes(),
    );
    let operation_progress_class = "required_write_content_shape_mismatch";
    let mut metadata = json!({
        "write_content_shape_mismatch": true,
        "success": false,
        "target": required_target,
        "observed_forbidden_markers": forbidden_markers,
        "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
        "operation_progress_class": operation_progress_class,
        "progress_effect": "no_progress",
        "active_targets": [required_target],
        "result_hash": result_hash,
        "tool_feedback_envelope": {
            "kind": "required_write_content_shape_mismatch",
            "success": false,
            "target": required_target,
            "side_effects_applied": false,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": operation_progress_class,
            "progress_effect": "no_progress",
            "active_targets": [required_target],
            "result_hash": result_hash
        },
        "terminal_guard_policy": {
            "owner": "tool_lifecycle_runtime",
            "no_progress_guard": true,
            "side_effects_applied": false
        }
    });
    if let Some(contract) = content_shape_contract
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("content_shape_contract".to_string(), contract.clone());
        if let Some(feedback) = object
            .get_mut("tool_feedback_envelope")
            .and_then(Value::as_object_mut)
        {
            feedback.insert("content_shape_contract".to_string(), contract);
        }
    }
    Some(ToolResult {
        title: "Required write content shape mismatch".to_string(),
        output_text: format!(
            "The submitted content does not match `{required_target}`'s contract. Runtime rejected this tool call before applying filesystem side effects.{content_shape_guidance}"
        ),
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    })
}

pub(crate) fn artifact_content_shape_violation_result(
    tool_name: &str,
    arguments: &Value,
    workspace_root: Option<&Utf8Path>,
) -> Option<ToolResult> {
    if tool_name == "write" {
        let target = arguments.get("path").and_then(Value::as_str)?;
        let normalized_target = canonical_artifact_content_shape_target(target, workspace_root);
        artifact_target_requires_content_shape(&normalized_target)?;
        let content = arguments.get("content").and_then(Value::as_str)?;
        if write_content_matches_required_target(&normalized_target, content) {
            return None;
        }
        return required_write_content_shape_violation_result(
            tool_name,
            arguments,
            &normalized_target,
        );
    }
    if tool_name != "apply_patch" {
        return None;
    }
    let patch_text = arguments.get("patch_text").and_then(Value::as_str)?;
    let (target, projected_content) =
        artifact_post_patch_content_shape_candidate(patch_text, workspace_root?)?;
    if write_content_matches_required_target(&target, &projected_content) {
        return None;
    }
    let mut projected_arguments = arguments.clone();
    if let Some(object) = projected_arguments.as_object_mut() {
        object.insert("path".to_string(), Value::String(target.clone()));
        object.insert("content".to_string(), Value::String(projected_content));
    }
    required_write_content_shape_violation_result(tool_name, &projected_arguments, &target)
}

pub(crate) fn artifact_target_requires_content_shape(target: &str) -> Option<()> {
    (language_artifact_target_requires_executable_shape(target).is_some()
        || code_artifact_target_requires_effective_shape(target)
        || text_artifact_target_requires_readable_shape(target))
    .then_some(())
}

fn language_artifact_target_requires_executable_shape(target: &str) -> Option<()> {
    (test_artifact_shape_contract(target).is_some()
        || source_artifact_shape_contract(target).is_some())
    .then_some(())
}

fn artifact_content_shape_metadata(target: &str) -> Option<Value> {
    if let Some(contract) = test_artifact_shape_contract(target) {
        return Some(contract.metadata_json());
    }
    if let Some(contract) = source_artifact_shape_contract(target) {
        return Some(contract.metadata_json());
    }
    if code_artifact_target_requires_effective_shape(target) {
        return Some(code_artifact_content_shape_metadata(target));
    }
    text_artifact_target_requires_readable_shape(target)
        .then(|| text_artifact_content_shape_metadata(target))
}

pub(crate) fn artifact_content_shape_metadata_for_feedback(target: &str) -> Value {
    artifact_content_shape_metadata(target).unwrap_or_else(|| {
        json!({
            "kind": "unknown_artifact_content_shape",
            "target": target.replace('\\', "/")
        })
    })
}

pub(crate) fn artifact_content_shape_prompt_contract(target: &str) -> Option<String> {
    if let Some(contract) = test_artifact_shape_contract(target) {
        return Some(contract.prompt_contract());
    }
    if text_artifact_target_requires_readable_shape(target) {
        return Some(text_artifact_prompt_contract(target));
    }
    if code_artifact_target_requires_effective_shape(target) {
        return Some(code_artifact_prompt_contract(target));
    }
    source_artifact_shape_contract(target).map(|contract| contract.prompt_contract())
}

pub(crate) fn artifact_content_shape_positive_guidance(target: &str) -> Option<String> {
    if let Some(contract) = test_artifact_shape_contract(target) {
        return Some(format!(
            "`{source}` is the inferred production source under test; do not rewrite `{source}` in this turn. The patch must create or update `{target}` as a complete test module only.{guidance}",
            source = contract.source_path,
            target = target.replace('\\', "/"),
            guidance = contract.positive_shape_guidance()
        ));
    }
    if text_artifact_target_requires_readable_shape(target) {
        return Some(text_artifact_positive_shape_guidance(target));
    }
    if code_artifact_target_requires_effective_shape(target) {
        return Some(code_artifact_positive_shape_guidance(target));
    }
    source_artifact_shape_contract(target).map(|contract| contract.positive_shape_guidance())
}

pub(crate) fn artifact_content_shape_tool_schema_description(target: &str) -> Option<String> {
    if let Some(contract) = test_artifact_shape_contract(target) {
        return Some(contract.tool_schema_description());
    }
    if text_artifact_target_requires_readable_shape(target) {
        return Some(text_artifact_tool_schema_description(target));
    }
    if code_artifact_target_requires_effective_shape(target) {
        return Some(code_artifact_tool_schema_description(target));
    }
    source_artifact_shape_contract(target).map(|contract| contract.tool_schema_description())
}

pub(crate) fn artifact_content_shape_apply_patch_recovery_scaffold(target: &str) -> Option<String> {
    test_artifact_shape_contract(target).map(|contract| contract.apply_patch_recovery_scaffold())
}

pub(crate) fn source_artifact_target_requires_executable_shape(target: &str) -> bool {
    source_artifact_shape_contract(target).is_some()
        || code_artifact_target_requires_effective_shape(target)
}

pub(crate) fn source_artifact_content_is_escaped_whole_file_string(
    target: &str,
    content: &str,
) -> bool {
    if source_artifact_shape_contract(target).is_some() {
        return language_source_artifact_content_is_escaped_whole_file_string(target, content);
    }
    code_artifact_target_requires_effective_shape(target)
        && code_artifact_content_is_serialized_string_snapshot(content)
}

fn artifact_post_patch_content_shape_candidate(
    patch_text: &str,
    workspace_root: &Utf8Path,
) -> Option<(String, String)> {
    let operations = PatchParser::parse(patch_text).ok()?;
    for operation in operations {
        match operation {
            PatchOperation::Add { path, contents } => {
                let target =
                    canonical_artifact_content_shape_target(path.as_str(), Some(workspace_root));
                if artifact_target_requires_content_shape(&target).is_some() {
                    return Some((target, contents));
                }
            }
            PatchOperation::Update {
                path,
                hunks,
                move_to,
            } => {
                let source =
                    canonical_artifact_content_shape_target(path.as_str(), Some(workspace_root));
                let target = move_to
                    .as_ref()
                    .map(|path| {
                        canonical_artifact_content_shape_target(path.as_str(), Some(workspace_root))
                    })
                    .unwrap_or_else(|| source.clone());
                if artifact_target_requires_content_shape(&target).is_none() {
                    continue;
                }
                let projected = if PatchParser::is_full_rewrite(&hunks) {
                    hunks
                        .first()
                        .map(|hunk| {
                            hunk.lines
                                .iter()
                                .filter_map(|line| match line {
                                    PatchLine::Context(value) | PatchLine::Insert(value) => {
                                        Some(value.clone())
                                    }
                                    PatchLine::Delete(_) => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default()
                } else {
                    let original =
                        std::fs::read_to_string(workspace_root.join(source.as_str()).as_std_path())
                            .ok()?;
                    PatchParser::apply_to_text(&original, &hunks).ok()?
                };
                return Some((target, projected));
            }
            PatchOperation::Delete { path } => {
                let target =
                    canonical_artifact_content_shape_target(path.as_str(), Some(workspace_root));
                if artifact_target_requires_content_shape(&target).is_some() {
                    return Some((target, String::new()));
                }
            }
        }
    }
    None
}

pub(crate) fn canonical_artifact_content_shape_target(
    target: &str,
    workspace_root: Option<&Utf8Path>,
) -> String {
    let normalized = crate::workspace::project::normalize_path_separators(target)
        .trim()
        .trim_matches('`')
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string();
    let Some(workspace_root) = workspace_root else {
        return normalized;
    };
    let root = crate::workspace::project::normalize_path_separators(workspace_root.as_str())
        .trim()
        .trim_end_matches('/')
        .to_string();
    if root.is_empty() || normalized.is_empty() {
        return normalized;
    }
    let normalized_key = normalized.to_ascii_lowercase();
    let root_key = root.to_ascii_lowercase();
    if normalized_key == root_key {
        return String::new();
    }
    let root_prefix = format!("{root_key}/");
    if normalized_key.starts_with(&root_prefix) {
        let relative_start = root.len() + 1;
        return normalized
            .get(relative_start..)
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
    }
    normalized
}

pub(crate) fn required_write_target_mismatch_content_shape_guidance(
    required_target: &str,
    requested_target: Option<&str>,
    submitted_content: &str,
) -> String {
    let Some(contract) = test_artifact_shape_contract(required_target) else {
        if let Some(contract) = source_artifact_shape_contract(required_target) {
            return format!(" {}", contract.positive_shape_guidance());
        }
        if text_artifact_target_requires_readable_shape(required_target) {
            return " Required positive text artifact shape: submit effective Markdown/text with real newline-separated document structure. Forbidden shape: do not send a quote-wrapped whole-document string, serialized string snapshot, escaped string literal, or content dominated by literal `\\n` escape sequences instead of real newlines.".to_string();
        }
        if code_artifact_target_requires_effective_shape(required_target) {
            return " Required positive code artifact shape: submit effective source or test code with real newline-separated code structure for the target language. Forbidden shape: do not send a quote-wrapped whole-file string, escaped serialized code, Markdown/spec prose, fenced code block wrapper, or content dominated by literal `\\n` escape sequences instead of real newlines.".to_string();
        }
        return String::new();
    };
    let requested_line = requested_target
        .filter(|target| *target == contract.source_path)
        .map(|target| {
            format!(" `{target}` is the production source under test, not the active write target.")
        })
        .unwrap_or_default();
    let observed_markers =
        language_test_artifact_forbidden_content_markers(required_target, submitted_content);
    let observed_line = if observed_markers.is_empty() {
        String::new()
    } else {
        format!(
            " Observed rejected content markers: {}.",
            observed_markers
                .iter()
                .map(|marker| format!("`{marker}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{requested_line}{}{}",
        contract.positive_shape_guidance(),
        observed_line
    )
}

pub(crate) fn write_content_matches_required_target(required_target: &str, content: &str) -> bool {
    language_artifact_content_has_executable_shape(required_target, content)
        && code_artifact_content_has_effective_shape(required_target, content)
        && text_artifact_content_has_readable_shape(required_target, content)
        && (test_artifact_shape_contract(required_target).is_none()
            || language_test_artifact_forbidden_content_markers(required_target, content)
                .is_empty())
        && (source_artifact_shape_contract(required_target).is_none()
            || language_source_artifact_forbidden_content_markers(required_target, content)
                .is_empty())
}

fn detected_content_shape_forbidden_markers(target: &str, content: &str) -> Vec<String> {
    if test_artifact_shape_contract(target).is_some() {
        return language_test_artifact_forbidden_content_markers(target, content);
    }
    if source_artifact_shape_contract(target).is_some() {
        return language_source_artifact_forbidden_content_markers(target, content);
    }
    if code_artifact_target_requires_effective_shape(target) {
        return detected_code_artifact_forbidden_content_markers(content);
    }
    if text_artifact_target_requires_readable_shape(target)
        && text_artifact_content_is_serialized_string_snapshot(content)
    {
        return vec!["serialized text artifact snapshot".to_string()];
    }
    Vec::new()
}

fn detected_code_artifact_forbidden_content_markers(content: &str) -> Vec<String> {
    let mut markers = Vec::new();
    if code_artifact_content_is_serialized_string_snapshot(content) {
        markers.push("quote-wrapped or escaped whole-file code string".to_string());
    }
    if code_artifact_content_is_markdown_or_prose_payload(content) {
        markers.push("Markdown/spec prose payload instead of effective code".to_string());
    }
    markers
}

pub(crate) fn text_artifact_content_shape_metadata(target: &str) -> Value {
    json!({
        "kind": "text_artifact_readable_content_shape",
        "target": target.replace('\\', "/"),
        "required_positive_shape": [
            "effective readable text artifact",
            "real newline-separated document structure",
            "Markdown/list/table/fence syntax appears as actual text, not escaped string literal data"
        ],
        "forbidden_shape": [
            "quote-wrapped whole-document string",
            "dominant literal \\\\n escape sequences instead of real newlines",
            "serialized string snapshot or escaped string literal"
        ]
    })
}

pub(crate) fn code_artifact_content_shape_metadata(target: &str) -> Value {
    json!({
        "kind": "generic_code_artifact_effective_content_shape",
        "target": target.replace('\\', "/"),
        "required_positive_shape": [
            "effective source or test code for the target language",
            "real newline-separated code structure",
            "no Markdown/spec prose wrapper"
        ],
        "forbidden_shape": [
            "quote-wrapped or escaped whole-file code string",
            "serialized code snapshot dominated by literal \\\\n escape sequences",
            "Markdown/spec prose payload or fenced code block wrapper instead of effective code"
        ]
    })
}

pub(crate) fn text_artifact_positive_shape_guidance(target: &str) -> String {
    format!(
        "Required positive text artifact shape for `{}`: submit effective Markdown/text content with real newline-separated document structure. Markdown headings, lists, tables, and fenced code blocks must appear as actual text, not as escaped string literal data. Forbidden shape: do not send a quote-wrapped whole-document string, serialized string snapshot, escaped string literal, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn text_artifact_prompt_contract(target: &str) -> String {
    let target = target.replace('\\', "/");
    format!(
        "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- The `content` must be Markdown/documentation text for `{target}` only.\n- Required positive text artifact shape: submit effective readable Markdown/text with real newline-separated document structure; headings, lists, tables, and fenced code blocks must be actual text.\n- Forbidden shape: do not send a quote-wrapped whole-document string, serialized string snapshot, escaped string literal, or content dominated by literal `\\n` escape sequences instead of real newlines.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
    )
}

pub(crate) fn text_artifact_tool_schema_description(target: &str) -> String {
    format!(
        "Complete final Markdown/text contents for `{}`. Required positive shape: effective readable document text with real newline-separated structure; headings, lists, tables, and fenced code blocks must be actual text. Do not send a quote-wrapped whole-document string, serialized string snapshot, escaped string literal, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn code_artifact_positive_shape_guidance(target: &str) -> String {
    format!(
        "Required positive code artifact shape for `{}`: submit effective source or test code for the target language with real newline-separated code structure. Forbidden shape: do not send a quote-wrapped whole-file string, escaped serialized code, Markdown/spec prose, fenced code block wrapper, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn code_artifact_prompt_contract(target: &str) -> String {
    let target = target.replace('\\', "/");
    format!(
        "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- The `content` must be effective source or test code for `{target}` in the target language.\n- Required positive code artifact shape: submit real newline-separated code structure, not a serialized string snapshot or Markdown/spec prose wrapper.\n- Forbidden shape: do not send a quote-wrapped whole-file string, escaped serialized code, Markdown/spec prose, fenced code block wrapper, or content dominated by literal `\\n` escape sequences instead of real newlines.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
    )
}

pub(crate) fn code_artifact_tool_schema_description(target: &str) -> String {
    format!(
        "Complete final source/test code contents for `{}`. Required positive code artifact shape: effective code for the target language with real newline-separated code structure. Do not send a quote-wrapped whole-file string, escaped serialized code, Markdown/spec prose, fenced code block wrapper, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn generic_code_artifact_content_shape_rejects_serialized_payload_fixture_passes() -> bool
{
    let target = "src/workflow.ts";
    let good = r#"
export function add(left: number, right: number): number {
  return left + right;
}
"#;
    let rust_good = r#"
pub fn render(value: &str) -> String {
    format!("value:{value}")
}
"#;
    let shell_good = r#"
#!/usr/bin/env bash
set -euo pipefail
echo "ready"
"#;
    let json_good = r#"
{
  "scripts": {
    "test": "vitest run"
  }
}
"#;
    let escaped = "\"export function add(left: number, right: number): number {\\n  return left + right;\\n}\\n\"";
    let markdown = r#"# workflow.ts

```ts
export function add(left: number, right: number): number {
  return left + right;
}
```
"#;
    let write_arguments = json!({
        "path": target,
        "content": escaped,
    });
    let prose = r#"
This file should implement the workflow.
It must expose the processing function for tests.
"#;
    code_artifact_target_requires_effective_shape(target)
        && code_artifact_content_has_effective_shape(target, good)
        && code_artifact_content_has_effective_shape("src/lib.rs", rust_good)
        && code_artifact_content_has_effective_shape("scripts/check.sh", shell_good)
        && code_artifact_content_has_effective_shape("package.json", json_good)
        && !code_artifact_content_has_effective_shape(target, escaped)
        && !code_artifact_content_has_effective_shape(target, markdown)
        && !code_artifact_content_has_effective_shape(target, prose)
        && write_content_matches_required_target(target, good)
        && write_content_matches_required_target("src/lib.rs", rust_good)
        && write_content_matches_required_target("scripts/check.sh", shell_good)
        && write_content_matches_required_target("package.json", json_good)
        && !write_content_matches_required_target(target, escaped)
        && artifact_content_shape_violation_result("write", &write_arguments, None).is_some()
        && artifact_content_shape_tool_schema_description("tests/workflow.spec.ts").is_some_and(
            |description| {
                description.contains("Required positive code artifact shape")
                    && !description.contains("unittest")
            },
        )
}

pub(crate) fn required_write_content_shape_mismatch_progress_class_fixture_passes() -> bool {
    let arguments = json!({
        "path": "test_workflow.py",
        "content": r#"
def main():
    print("not a test module")
"#,
    });
    let Some(result) = required_write_content_shape_violation_result_with_requested_target(
        "write",
        &arguments,
        "test_workflow.py",
        Some("workflow.py"),
    ) else {
        return false;
    };
    let Some(adapter_contract) = test_artifact_shape_contract("test_workflow.py") else {
        return false;
    };
    let adapter_metadata = adapter_contract.metadata_json();
    result.metadata["operation_progress_class"] == "required_write_content_shape_mismatch"
        && result.metadata["progress_effect"] == "no_progress"
        && result.metadata["tool_feedback_envelope"]["kind"]
            == "required_write_content_shape_mismatch"
        && result.metadata["tool_feedback_envelope"]["operation_progress_class"]
            == "required_write_content_shape_mismatch"
        && result.metadata["tool_feedback_envelope"]["progress_effect"] == "no_progress"
        && result.metadata["tool_feedback_envelope"]["side_effects_applied"] == false
        && result.metadata["content_shape_contract"]["kind"] == adapter_metadata["kind"]
        && result.metadata["tool_feedback_envelope"]["content_shape_contract"]["kind"]
            == adapter_metadata["kind"]
        && result
            .output_text
            .contains("Runtime rejected this tool call before applying filesystem side effects")
}

#[cfg(test)]
pub(crate) fn generated_test_recovery_scaffold_fixture_passes() -> bool {
    let target = "test_workflow.py";
    let Some(schema) = artifact_content_shape_tool_schema_description(target) else {
        return false;
    };
    let Some(scaffold) = artifact_content_shape_apply_patch_recovery_scaffold(target) else {
        return false;
    };
    schema.contains("Generated-test recovery scaffold")
        && schema.contains("*** Add File: test_workflow.py")
        && schema.contains("import `workflow`")
        && schema.contains("class TestWorkflow(unittest.TestCase)")
        && scaffold.contains("Positive generated-test apply_patch scaffold")
        && scaffold.contains("`*** Add File: test_workflow.py`")
        && scaffold.contains("import `workflow`")
        && scaffold.contains("`+class TestWorkflow(unittest.TestCase):`")
        && scaffold.contains("`+    def test_<requested_behavior>(self):`")
        && scaffold.contains("do not paste implementation code from `workflow.py`")
        && !scaffold.contains("calculator")
}

pub(crate) fn text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes() -> bool {
    let good =
        "# Workflow Design\n\n## Tests\n\n- `tests/workflow.spec.ts` covers public behavior.\n";
    let escaped = "\"# Workflow Design\\n\\n## Tests\\n\\n- `tests/workflow.spec.ts` covers public behavior.\\n\\n```\\nverify-workflow --behavior\\n```\\n\"";
    text_artifact_content_has_readable_shape("docs/workflow-design.md", good)
        && !text_artifact_content_has_readable_shape("docs/workflow-design.md", escaped)
        && !text_artifact_content_is_serialized_string_snapshot(good)
        && text_artifact_content_is_serialized_string_snapshot(escaped)
}

pub(crate) fn text_artifact_readable_shape_rejects_short_serialized_markdown_fixture_passes() -> bool
{
    let target = "docs/readme.md";
    let good = "# README\n\nUsage\n";
    let escaped = "\"# README\\n\\nUsage\\n\"";
    let write_arguments = json!({
        "path": target,
        "content": escaped,
    });
    text_artifact_content_has_readable_shape(target, good)
        && !text_artifact_content_has_readable_shape(target, escaped)
        && !write_content_matches_required_target(target, escaped)
        && artifact_content_shape_violation_result("write", &write_arguments, None).is_some()
}

pub(crate) fn text_artifact_content_shape_rejects_serialized_markdown_fixture_passes() -> bool {
    let bad_arguments = json!({
        "path": "docs/workflow-design.md",
        "content": "\"# Workflow Design\\n\\n## Tests\\n\\n- `tests/workflow.behavior.md` covers public behavior.\\n\\n```\\nverify-contract --behavior\\n```\\n\""
    });
    let good_arguments = json!({
        "path": "docs/workflow-design.md",
        "content": "# Workflow Design\n\n## Tests\n\n- `tests/workflow.behavior.md` covers public behavior.\n\n```bash\nverify-contract --behavior\n```\n"
    });
    let root_path = std::env::temp_dir().join(format!(
        "moyai-text-shape-patch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    if std::fs::create_dir_all(root.as_std_path()).is_err() {
        return false;
    }
    let patch_arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: docs/workflow-design.md\n+\"# Workflow Design\\n\\n## Tests\\n\\n- `tests/workflow.behavior.md` covers public behavior.\\n\\n```\\nverify-contract --behavior\\n```\\n\"\n*** End Patch"
    });
    let Some(bad_result) = artifact_content_shape_violation_result("write", &bad_arguments, None)
    else {
        let _ = std::fs::remove_dir_all(root.as_std_path());
        return false;
    };
    let patch_rejected = artifact_content_shape_violation_result(
        "apply_patch",
        &patch_arguments,
        Some(root.as_path()),
    )
    .is_some_and(|result| {
        result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("text_artifact_readable_content_shape")
    });
    let patch_left_workspace_clean = !root.join("docs/workflow-design.md").exists();
    let _ = std::fs::remove_dir_all(root.as_std_path());
    artifact_content_shape_violation_result("write", &good_arguments, None).is_none()
        && bad_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && bad_result
            .metadata
            .pointer("/content_shape_contract/kind")
            .and_then(Value::as_str)
            == Some("text_artifact_readable_content_shape")
        && bad_result
            .output_text
            .contains("Required positive text artifact shape")
        && patch_rejected
        && patch_left_workspace_clean
        && text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes()
}

pub(crate) fn content_shape_mismatch_canonicalizes_workspace_absolute_target_fixture_passes() -> bool
{
    let root = Utf8PathBuf::from("C:/workspace");
    let bad_content = "\"# Workflow Design\\n\\n## Tests\\n\\n- `tests/workflow.behavior.md` covers public behavior.\\n\"";
    let absolute_arguments = json!({
        "path": r"C:\\workspace\\docs\\workflow-design.md",
        "content": bad_content
    });
    let relative_arguments = json!({
        "path": "docs/workflow-design.md",
        "content": bad_content
    });
    let Some(absolute_result) =
        artifact_content_shape_violation_result("write", &absolute_arguments, Some(root.as_path()))
    else {
        return false;
    };
    let Some(relative_result) =
        artifact_content_shape_violation_result("write", &relative_arguments, Some(root.as_path()))
    else {
        return false;
    };
    let metadata_target = absolute_result
        .metadata
        .pointer("/content_shape_contract/target")
        .and_then(Value::as_str);
    let feedback_target = absolute_result
        .metadata
        .pointer("/tool_feedback_envelope/target")
        .and_then(Value::as_str);
    let active_target = absolute_result
        .metadata
        .pointer("/active_targets/0")
        .and_then(Value::as_str);
    let absolute_hash = absolute_result
        .metadata
        .pointer("/result_hash")
        .and_then(Value::as_str);
    let relative_hash = relative_result
        .metadata
        .pointer("/result_hash")
        .and_then(Value::as_str);
    metadata_target == Some("docs/workflow-design.md")
        && feedback_target == Some("docs/workflow-design.md")
        && active_target == Some("docs/workflow-design.md")
        && absolute_hash.is_some()
        && absolute_hash == relative_hash
        && absolute_result
            .output_text
            .contains("`docs/workflow-design.md`")
        && !absolute_result.output_text.contains("C:/workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_artifact_readable_shape_rejects_short_serialized_markdown() {
        assert!(text_artifact_readable_shape_rejects_short_serialized_markdown_fixture_passes());
    }

    #[test]
    fn generated_test_recovery_scaffold_is_positive_and_workflow_neutral() {
        assert!(generated_test_recovery_scaffold_fixture_passes());
    }
}

pub(crate) fn source_content_shape_rejects_escaped_whole_file_fixture_passes() -> bool {
    let good = r#"
import math

def square(value):
    return value * value

if __name__ == "__main__":
    print(square(3))
"#;
    let escaped = "\"import math\\n\\ndef square(value):\\n    return value * value\\n\\nif __name__ == \\\"__main__\\\":\\n    print(square(3))\\n\"";
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && !language_source_artifact_content_has_executable_shape("workflow.py", escaped)
        && language_artifact_content_has_executable_shape("workflow.py", good)
        && !language_artifact_content_has_executable_shape("workflow.py", escaped)
}

pub(crate) fn source_content_shape_rejects_test_module_payload_fixture_passes() -> bool {
    let good = r#"
def add(left, right):
    return left + right

if __name__ == "__main__":
    print(add(2, 3))
"#;
    let test_payload = r#"
import unittest
import workflow

class TestWorkflow(unittest.TestCase):
    def test_add(self):
        self.assertEqual(workflow.add(2, 3), 5)

if __name__ == "__main__":
    unittest.main()
"#;
    let mentions_tests_only = r#"
def describe():
    return "tests should cover this module"
"#;
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && language_artifact_content_has_executable_shape("workflow.py", good)
        && !language_source_artifact_content_has_executable_shape("workflow.py", test_payload)
        && !language_artifact_content_has_executable_shape("workflow.py", test_payload)
        && language_source_artifact_forbidden_content_markers("workflow.py", test_payload)
            .iter()
            .any(|marker| marker.contains("test module payload"))
        && language_source_artifact_forbidden_content_markers("workflow.py", mentions_tests_only)
            .is_empty()
        && language_source_artifact_content_has_executable_shape("workflow.py", mentions_tests_only)
}

pub(crate) fn source_content_shape_rejects_markdown_payload_fixture_passes() -> bool {
    let good = r#"
def transform_record(value):
    normalized = value.strip()
    return normalized or "empty"

if __name__ == "__main__":
    print(transform_record(" ready "))
"#;
    let markdown_payload = r#"# workflow.py

## Source Shape Notes

- Entrypoint guard: `if __name__ == "__main__"`
- Public function: `transform_record`

```text
example output
```
"#;
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && !language_source_artifact_content_has_executable_shape("workflow.py", markdown_payload)
        && !language_artifact_content_has_executable_shape("workflow.py", markdown_payload)
        && language_source_artifact_forbidden_content_markers("workflow.py", markdown_payload)
            .iter()
            .any(|marker| marker.contains("Markdown/prose payload"))
}

pub(crate) fn source_content_shape_rejects_raw_prose_line_fixture_passes() -> bool {
    let good = r#"
def transform_record(value):
    normalized = value.strip()
    return normalized or "empty"

if __name__ == "__main__":
    print(transform_record(" ready "))
"#;
    let raw_prose_between_code = r#"
# Source module overview

This module normalizes one input value and prints a fallback for empty input.

def transform_record(value):
    normalized = value.strip()
    return normalized or "empty"
"#;
    let commented_japanese = r#"
# Source module overview
# This module normalizes one input value and prints a fallback for empty input.

def transform_record(value):
    normalized = value.strip()
    return normalized or "empty"
"#;
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && !language_source_artifact_content_has_executable_shape(
            "workflow.py",
            raw_prose_between_code,
        )
        && !language_artifact_content_has_executable_shape("workflow.py", raw_prose_between_code)
        && language_source_artifact_content_has_executable_shape("workflow.py", commented_japanese)
        && language_artifact_content_has_executable_shape("workflow.py", commented_japanese)
        && !language_source_artifact_content_has_executable_shape(
            "workflow.py",
            r#"
def transform_record(value):
    normalized = value.strip()
    return normalized or "empty"

Usage:
    run the module with a sample value
"#,
        )
}

pub(crate) fn source_content_shape_rejects_duplicate_entrypoint_fixture_passes() -> bool {
    let good = r#"
import sys

def main():
    print("ok")

if __name__ == "__main__":
    main()
"#;
    let concatenated = r#"
import sys

def main():
    print("first")

if __name__ == "__main__":
    main()

import sys

def main():
    print("second")

if __name__ == "__main__":
    main()
"#;
    let write_arguments = json!({
        "path": "workflow.py",
        "content": concatenated,
    });
    let Some(write_result) =
        artifact_content_shape_violation_result("write", &write_arguments, None)
    else {
        return false;
    };
    let root_path = std::env::temp_dir().join(format!(
        "moyai-source-duplicate-entrypoint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = camino::Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    if std::fs::create_dir_all(root.as_std_path()).is_err()
        || std::fs::write(root.join("workflow.py").as_std_path(), good).is_err()
    {
        let _ = std::fs::remove_dir_all(root.as_std_path());
        return false;
    }
    let patch_arguments = json!({
        "patch_text": "*** Begin Patch\n*** Update File: workflow.py\n@@\n if __name__ == \"__main__\":\n     main()\n+\n+if __name__ == \"__main__\":\n+    main()\n*** End Patch"
    });
    let patch_result =
        artifact_content_shape_violation_result("apply_patch", &patch_arguments, Some(&root));
    let patch_left_workspace_clean =
        std::fs::read_to_string(root.join("workflow.py").as_std_path())
            .ok()
            .as_deref()
            == Some(good);
    let _ = std::fs::remove_dir_all(root.as_std_path());
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && language_artifact_content_has_executable_shape("workflow.py", good)
        && !language_source_artifact_content_has_executable_shape("workflow.py", concatenated)
        && !language_artifact_content_has_executable_shape("workflow.py", concatenated)
        && language_source_artifact_forbidden_content_markers("workflow.py", good).is_empty()
        && language_source_artifact_forbidden_content_markers("workflow.py", concatenated)
            .iter()
            .any(|marker| marker == &["multiple executable entrypoint", "guards"].join(" "))
        && write_result
            .metadata
            .pointer("/content_shape_contract/forbidden_shape")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().filter_map(Value::as_str).any(|item| {
                    item.contains(&["multiple executable entrypoint", "guards"].join(" "))
                })
            })
        && write_result
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && patch_result.is_some()
        && patch_left_workspace_clean
}

pub(crate) fn source_executable_shape_accepts_required_public_surface_fixture_passes() -> bool {
    let good = r#"
class PublicWorkflow:
    def __init__(self):
        self.enabled = True

    @staticmethod
    def format_public_record(record):
        return format_public_record(record)


def format_public_record(record):
    label = record.get("label", "").strip()
    status = record.get("status", "draft").strip()
    return f"{label}:{status}"


def record_has_required_fields(record):
    return (
        "label" in record and record.get("label") != ""
        and "status" in record and record.get("status") != ""
    )


def _prepare_record(label, status):
    return {
        "label": label.strip(),
        "status": status.strip(),
    }


def _run_internal_helper():
    workflow = PublicWorkflow()
    prepared = _prepare_record(
        " item ",
        " ready ",
    )
    workflow.format_public_record(prepared)
    print(record_has_required_fields(prepared))


if __name__ == "__main__":
    _run_internal_helper()
"#;
    let arguments = json!({
        "path": "workflow.py",
        "content": good,
    });
    language_source_artifact_content_has_executable_shape("workflow.py", good)
        && language_artifact_content_has_executable_shape("workflow.py", good)
        && write_content_matches_required_target("workflow.py", good)
        && artifact_content_shape_violation_result("write", &arguments, None).is_none()
}

pub(crate) fn content_shape_contract_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    generic_code_artifact_content_shape_rejects_serialized_payload_fixture_passes()
        && required_write_content_shape_mismatch_progress_class_fixture_passes()
        && text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes()
        && source_content_shape_rejects_escaped_whole_file_fixture_passes()
        && source_content_shape_rejects_test_module_payload_fixture_passes()
        && source_content_shape_rejects_markdown_payload_fixture_passes()
        && source_content_shape_rejects_raw_prose_line_fixture_passes()
        && source_content_shape_rejects_duplicate_entrypoint_fixture_passes()
        && source_executable_shape_accepts_required_public_surface_fixture_passes()
        && test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes()
        && test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
}

pub(crate) fn test_target_content_shape_projection_is_positive_and_forbidden() -> bool {
    crate::agent::language_evidence::test_target_content_shape_projection_is_positive_and_forbidden(
    )
}

pub(crate) fn test_target_subprocess_returncode_assertion_diagnostics_fixture_passes() -> bool {
    crate::agent::language_evidence::test_target_subprocess_returncode_assertion_diagnostics_fixture_passes()
}

pub(crate) fn test_target_module_qualified_reference_import_fixture_passes() -> bool {
    crate::agent::language_evidence::test_target_module_qualified_reference_import_fixture_passes()
}

pub(crate) fn test_target_rejects_recursive_runner_self_invocation_fixture_passes() -> bool {
    crate::agent::language_evidence::test_target_rejects_recursive_runner_self_invocation_fixture_passes()
}

pub(crate) fn test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes() -> bool {
    crate::agent::language_evidence::test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes()
}

pub(crate) fn test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
-> bool {
    crate::agent::language_evidence::test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
}
