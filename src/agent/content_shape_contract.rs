use std::collections::BTreeSet;

use camino::Utf8Path;
use serde_json::{Value, json};

use crate::edit::{PatchLine, PatchOperation, PatchParser};
use crate::protocol::OperationIntent;
use crate::tool::ToolResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestTargetContentShapeContract {
    pub(crate) target: String,
    pub(crate) source_path: String,
    pub(crate) module_name: String,
    pub(crate) class_name: String,
}

pub(crate) fn python_source_for_test_target(
    target: &str,
) -> Option<TestTargetContentShapeContract> {
    let normalized = target.replace('\\', "/");
    let (dir, name) = normalized
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name.to_string()))
        .unwrap_or_else(|| (String::new(), normalized.clone()));
    let stem = name.strip_suffix(".py")?;
    let module = stem
        .strip_prefix("test_")
        .or_else(|| stem.strip_suffix("_test"))?;
    if module.trim().is_empty() {
        return None;
    }
    Some(TestTargetContentShapeContract {
        target: normalized,
        source_path: format!("{dir}{module}.py"),
        module_name: module.to_string(),
        class_name: format!("Test{}", snake_to_pascal(module)),
    })
}

pub(crate) fn python_test_module_content_has_executable_shape(target: &str, content: &str) -> bool {
    let Some(contract) = python_source_for_test_target(target) else {
        return true;
    };
    let executable = python_code_without_strings_or_comments(content);
    let lower = executable.to_ascii_lowercase();
    let module = contract.module_name.to_ascii_lowercase();
    let has_test_definition = lower.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def test_")
            || trimmed.starts_with("async def test_")
            || trimmed.starts_with("class test")
    });
    let has_test_context = lower.contains("import unittest")
        || lower.contains("from unittest")
        || lower.contains("import pytest")
        || lower.contains(&format!("import {module}"))
        || lower.contains(&format!("from {module} import"));
    has_test_definition
        && has_test_context
        && !test_target_has_invalid_test_class_base(&executable)
        && !test_target_has_missing_module_import_for_qualified_reference(&executable)
        && !test_target_has_opaque_subprocess_returncode_assertion(content)
}

pub(crate) fn python_source_target_requires_executable_shape(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".py") && python_source_for_test_target(&normalized).is_none()
}

pub(crate) fn python_source_content_has_executable_shape(target: &str, content: &str) -> bool {
    if !python_source_target_requires_executable_shape(target) {
        return true;
    }
    !python_source_content_is_escaped_whole_file_string(content)
        && !python_source_content_is_test_module_payload(content)
        && !python_source_content_is_markdown_or_prose_payload(content)
        && !python_source_content_has_raw_prose_line(content)
        && python_source_content_has_code_shape(content)
}

pub(crate) fn python_artifact_content_has_executable_shape(target: &str, content: &str) -> bool {
    if python_source_for_test_target(target).is_some() {
        return python_test_module_content_has_executable_shape(target, content);
    }
    python_source_content_has_executable_shape(target, content)
}

pub(crate) fn text_artifact_target_requires_readable_shape(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".md")
        || normalized.ends_with(".markdown")
        || normalized.ends_with(".txt")
        || normalized.ends_with(".rst")
}

pub(crate) fn text_artifact_content_has_readable_shape(target: &str, content: &str) -> bool {
    if !text_artifact_target_requires_readable_shape(target) {
        return true;
    }
    !text_artifact_content_is_serialized_string_snapshot(content)
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
    if escaped_newlines < 4 || escaped_newlines <= real_newlines.saturating_mul(3) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("\\n#")
        || lower.contains("\\n##")
        || lower.contains("\\n- ")
        || lower.contains("\\n* ")
        || lower.contains("\\n```")
        || lower.contains("\\n|")
}

pub(crate) fn required_write_content_shape_violation_result(
    tool_name: &str,
    arguments: &Value,
    required_target: &str,
) -> Option<ToolResult> {
    if !matches!(tool_name, "write" | "apply_patch") {
        return None;
    }
    let content = arguments.get("content").and_then(Value::as_str)?;
    let content_shape_guidance =
        required_write_target_mismatch_content_shape_guidance(required_target, None, content);
    let content_shape_contract = artifact_content_shape_metadata(required_target);
    let forbidden_markers = detected_test_target_forbidden_content_markers(content);
    let result_hash = crate::harness::artifact::hash_bytes(
        format!("required_write_content_shape_mismatch:{required_target}").as_bytes(),
    );
    let mut metadata = json!({
        "write_content_shape_mismatch": true,
        "success": false,
        "target": required_target,
        "observed_forbidden_markers": forbidden_markers,
        "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
        "operation_progress_class": "no_progress",
        "progress_effect": "no_progress",
        "active_targets": [required_target],
        "result_hash": result_hash,
        "tool_feedback_envelope": {
            "kind": "required_write_content_shape_mismatch",
            "success": false,
            "target": required_target,
            "side_effects_applied": false,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "no_progress",
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
    (python_artifact_target_requires_executable_shape(target).is_some()
        || text_artifact_target_requires_readable_shape(target))
    .then_some(())
}

fn python_artifact_target_requires_executable_shape(target: &str) -> Option<()> {
    (python_source_for_test_target(target).is_some()
        || python_source_target_requires_executable_shape(target))
    .then_some(())
}

fn artifact_content_shape_metadata(target: &str) -> Option<Value> {
    if let Some(contract) = python_source_for_test_target(target) {
        return Some(contract.metadata_json());
    }
    if python_source_target_requires_executable_shape(target) {
        return Some(python_source_content_shape_metadata(target));
    }
    text_artifact_target_requires_readable_shape(target)
        .then(|| text_artifact_content_shape_metadata(target))
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
    let Some(contract) = python_source_for_test_target(required_target) else {
        if python_source_target_requires_executable_shape(required_target) {
            return " Required positive source shape: submit effective Python module text with real newline-separated source structure. Forbidden shape: do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, or a unittest/pytest test module payload.".to_string();
        }
        if text_artifact_target_requires_readable_shape(required_target) {
            return " Required positive text artifact shape: submit effective Markdown/text with real newline-separated document structure. Forbidden shape: do not send a quote-wrapped whole-document string, JSON/Python-escaped serialized Markdown, or content dominated by literal `\\n` escape sequences instead of real newlines.".to_string();
        }
        return String::new();
    };
    let requested_line = requested_target
        .filter(|target| *target == contract.source_path)
        .map(|target| {
            format!(" `{target}` is the production source under test, not the active write target.")
        })
        .unwrap_or_default();
    let observed_markers = detected_test_target_forbidden_content_markers(submitted_content);
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
    python_artifact_content_has_executable_shape(required_target, content)
        && text_artifact_content_has_readable_shape(required_target, content)
        && (python_source_for_test_target(required_target).is_none()
            || detected_test_target_forbidden_content_markers(content).is_empty())
}

pub(crate) fn detected_test_target_forbidden_content_markers(content: &str) -> Vec<String> {
    let mut markers = BTreeSet::new();
    if contains_direct_input_call(content) {
        markers.insert("input(".to_string());
    }
    if test_target_has_invalid_test_class_base(&python_code_without_strings_or_comments(content)) {
        markers.insert("class Test* missing unittest.TestCase base".to_string());
    }
    for line in content.lines() {
        let trimmed = line.trim_start();
        let is_top_level = line.len() == trimmed.len();
        let Some(rest) = trimmed.strip_prefix("def ") else {
            continue;
        };
        let name = rest
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .next()
            .unwrap_or_default();
        if name == "main" || (is_top_level && !name.is_empty() && !name.starts_with("test_")) {
            markers.insert(format!("def {name}"));
        }
    }
    markers.into_iter().collect()
}

fn test_target_has_invalid_test_class_base(executable_content: &str) -> bool {
    executable_content.lines().any(|line| {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("class ") else {
            return false;
        };
        let class_name = rest
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .next()
            .unwrap_or_default();
        if !class_name.starts_with("Test") {
            return false;
        }
        let Some(base_start) = rest.find('(') else {
            return true;
        };
        let Some(base_end) = rest[base_start + 1..].find(')') else {
            return true;
        };
        let bases = &rest[base_start + 1..base_start + 1 + base_end];
        !bases.split(',').any(|base| {
            let base_key = base
                .trim()
                .trim_start_matches("unittest.")
                .replace(' ', "")
                .to_ascii_lowercase();
            base_key.ends_with("testcase")
        })
    })
}

fn contains_direct_input_call(content: &str) -> bool {
    let bytes = content.as_bytes();
    let needle = b"input";
    let mut index = 0usize;
    while let Some(offset) = content[index..].to_ascii_lowercase().find("input") {
        let start = index + offset;
        let end = start + needle.len();
        let previous = start.checked_sub(1).and_then(|idx| bytes.get(idx).copied());
        if previous.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
        {
            index = end;
            continue;
        }
        let mut cursor = end;
        while let Some(byte) = bytes.get(cursor) {
            if !byte.is_ascii_whitespace() {
                break;
            }
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'(') {
            return true;
        }
        index = end;
    }
    false
}

pub(crate) fn python_source_content_is_escaped_whole_file_string(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let real_newlines = trimmed.matches('\n').count();
    let escaped_newlines = trimmed.matches("\\n").count();
    if escaped_newlines < 4 || escaped_newlines <= real_newlines.saturating_mul(3) {
        return false;
    }
    let quote_wrapped = (trimmed.starts_with("\"\"\"") && trimmed.ends_with("\"\"\""))
        || (trimmed.starts_with("'''") && trimmed.ends_with("'''"))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
    if !quote_wrapped {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("\\ndef ")
        || lower.contains("\\nclass ")
        || lower.contains("\\nimport ")
        || lower.contains("\\nfrom ")
        || lower.contains("\\nif __name__")
}

pub(crate) fn python_source_content_is_test_module_payload(content: &str) -> bool {
    let executable = python_code_without_strings_or_comments(content);
    let lower = executable.to_ascii_lowercase();
    let has_test_definition = lower.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def test_")
            || trimmed.starts_with("async def test_")
            || trimmed.starts_with("class test")
    });
    let has_test_framework_context = lower.contains("import unittest")
        || lower.contains("from unittest")
        || lower.contains("unittest.")
        || lower.contains("import pytest")
        || lower.contains("from pytest")
        || lower.contains("pytest.");
    let has_test_runner = lower.contains("unittest.main(") || lower.contains("pytest.main(");
    has_test_runner || (has_test_definition && has_test_framework_context)
}

pub(crate) fn python_source_content_is_markdown_or_prose_payload(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let code_shape = python_source_content_has_code_shape(trimmed);
    let lower = trimmed.to_ascii_lowercase();
    let has_markdown_structure = trimmed.contains("```")
        || trimmed.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("# ") || line.starts_with("## ") || line.starts_with("- ")
        })
        || lower.contains("## requirements")
        || lower.contains("## usage")
        || lower.contains("## 要件")
        || lower.contains("## 実行方法");
    let prose_lines = trimmed
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with('-')
                && !line.starts_with("```")
                && !python_source_line_has_code_shape(line)
        })
        .count();
    (has_markdown_structure && !code_shape) || (prose_lines >= 2 && !code_shape)
}

fn python_source_content_has_raw_prose_line(content: &str) -> bool {
    let executable = python_code_without_strings_or_comments(content);
    executable.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !python_source_line_can_be_executable_python(trimmed)
            && python_source_line_looks_like_prose(trimmed)
    })
}

fn python_source_content_has_code_shape(content: &str) -> bool {
    let executable = python_code_without_strings_or_comments(content);
    executable
        .lines()
        .any(|line| python_source_line_has_code_shape(line.trim_start()))
}

fn python_source_line_has_code_shape(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    lower.starts_with("def ")
        || lower.starts_with("async def ")
        || lower.starts_with("class ")
        || lower.starts_with("import ")
        || lower.starts_with("from ")
        || lower.starts_with("if ")
        || lower.starts_with("if __name__")
        || lower.starts_with("print(")
        || lower.starts_with("raise ")
        || lower.starts_with("return ")
        || lower.starts_with("while ")
        || lower.starts_with("for ")
        || lower.starts_with("try:")
        || lower.starts_with("with ")
        || lower.starts_with("@")
        || python_source_line_has_assignment_shape(line)
}

fn python_source_line_can_be_executable_python(line: &str) -> bool {
    if python_source_line_has_code_shape(line) {
        return true;
    }
    let lower = line.to_ascii_lowercase();
    lower == "pass"
        || lower == "break"
        || lower == "continue"
        || lower == "else:"
        || lower == "finally:"
        || lower.starts_with("elif ")
        || lower.starts_with("except")
        || lower.starts_with("case ")
        || lower.starts_with("match ")
        || lower.starts_with("assert ")
        || lower.starts_with("del ")
        || lower.starts_with("global ")
        || lower.starts_with("nonlocal ")
        || lower.starts_with("yield ")
        || lower.starts_with("sys.exit(")
        || lower.starts_with("exit(")
        || lower.starts_with("main(")
        || lower.starts_with("unittest.main(")
        || matches!(line, ")" | "]" | "}" | ")," | "]," | "},")
        || line.starts_with('.')
        || line.starts_with(')')
        || line.starts_with(']')
        || line.starts_with('}')
}

fn python_source_line_looks_like_prose(line: &str) -> bool {
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

fn python_source_line_has_assignment_shape(line: &str) -> bool {
    if line.starts_with('-') || line.contains('`') {
        return false;
    }
    if line.contains("==") || line.contains("!=") || line.contains("<=") || line.contains(">=") {
        return false;
    }
    line.contains('=')
}

fn python_code_without_strings_or_comments(content: &str) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(content.len());
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '#' {
            while index < chars.len() && chars[index] != '\n' {
                output.push(' ');
                index += 1;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let triple =
                index + 2 < chars.len() && chars[index + 1] == quote && chars[index + 2] == quote;
            let terminator_len = if triple { 3 } else { 1 };
            for _ in 0..terminator_len {
                output.push(' ');
                index += 1;
            }
            while index < chars.len() {
                if !triple && chars[index] == '\\' {
                    output.push(' ');
                    index += 1;
                    if index < chars.len() {
                        output.push(if chars[index] == '\n' { '\n' } else { ' ' });
                        index += 1;
                    }
                    continue;
                }
                if triple
                    && index + 2 < chars.len()
                    && chars[index] == quote
                    && chars[index + 1] == quote
                    && chars[index + 2] == quote
                {
                    for _ in 0..3 {
                        output.push(' ');
                        index += 1;
                    }
                    break;
                }
                if !triple && chars[index] == quote {
                    output.push(' ');
                    index += 1;
                    break;
                }
                output.push(if chars[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
            continue;
        }
        output.push(ch);
        index += 1;
    }
    output
}

fn test_target_has_opaque_subprocess_returncode_assertion(content: &str) -> bool {
    if !content.contains("subprocess.run") || !content.contains(".returncode") {
        return false;
    }
    let lines = content.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("assert") || !lower.contains(".returncode") {
            continue;
        }
        let mut statement = String::new();
        let mut paren_balance = 0i32;
        for statement_line in lines.iter().skip(index).take(12) {
            statement.push_str(statement_line);
            statement.push('\n');
            for ch in statement_line.chars() {
                match ch {
                    '(' => paren_balance += 1,
                    ')' => paren_balance -= 1,
                    _ => {}
                }
            }
            if paren_balance <= 0 && statement.trim_end().ends_with(')') {
                break;
            }
            if paren_balance <= 0 && !statement_line.trim_end().ends_with('\\') {
                break;
            }
        }
        let statement_lower = statement.to_ascii_lowercase();
        if !(statement_lower.contains(".stdout") || statement_lower.contains(".stderr")) {
            return true;
        }
    }
    false
}

fn test_target_has_missing_module_import_for_qualified_reference(executable: &str) -> bool {
    ["sys", "os", "subprocess", "unittest"]
        .iter()
        .any(|module| {
            executable.contains(&format!("{module}."))
                && !python_module_imported_as_module(executable, module)
        })
}

fn python_module_imported_as_module(executable: &str, module: &str) -> bool {
    executable.lines().any(|line| {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("import ") else {
            return false;
        };
        rest.split(',').any(|item| {
            let imported = item
                .trim()
                .split(" as ")
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            imported == module || imported.starts_with(&format!("{module}."))
        })
    })
}

impl TestTargetContentShapeContract {
    pub(crate) fn positive_shape_guidance(&self) -> String {
        format!(
            " Required positive test-module shape for `{target}`: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert behavior by calling `{module}.<public_function>(...)` or imported public functions. If the current request/spec includes public command examples for `{source}`, cover them with subprocess-style argv tests that assert return code and observable stdout/stderr; every generated `subprocess.run(...)` child command must pass a finite `timeout=`, any assertion that reads `CompletedProcess.stdout` or `.stderr` must use `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE`, every returncode assertion for a subprocess result must include captured stdout/stderr diagnostics in its failure message, module-qualified helper references such as `sys.executable` must import that module, and parent-side UTF-8 text decoding such as `encoding=\"utf-8\"` must give the child explicit UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`. Optional launch block is allowed only as `if __name__ == \"__main__\": unittest.main()`. Forbidden shape: do not define production functions at module top level, do not define `main()`, do not use requirement ids such as `FILE_ID` / `API_ID` / `BEH_ID` as Python class bases, do not over-specify concrete exception classes or localized exception messages unless the current contract names them, do not directly call `input(...)` as implementation logic, and do not paste implementation code from `{source}`.",
            target = self.target,
            module = self.module_name,
            source = self.source_path
        )
    }

    pub(crate) fn prompt_contract(&self) -> String {
        format!(
            "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- `{source}` is the inferred production source under test; do not rewrite `{source}` in this turn.\n- The `content` must be a complete test module for `{target}` only.\n- Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions.\n- Public command coverage: when the prompt/spec includes public command examples for `{source}`, add subprocess-style tests that execute the requested argv forms and assert return code plus stdout/stderr behavior. Every generated `subprocess.run(...)` child command must pass a finite `timeout=` so verification cannot block indefinitely, any test that reads `CompletedProcess.stdout` or `.stderr` must capture those streams with `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE`, every returncode assertion for a subprocess result must include captured stdout/stderr diagnostics in its failure message, module-qualified helper references such as `sys.executable` must import that module, and parent-side UTF-8 text decoding such as `encoding=\"utf-8\"` must pass explicit child UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`.\n- Allowed launch block: `if __name__ == \"__main__\": unittest.main()`.\n- Forbidden shape: do not define production functions at module top level, do not define `main()`, do not use requirement ids such as `FILE_ID` / `API_ID` / `BEH_ID` as Python class bases, do not over-specify concrete exception classes or localized exception messages unless the current contract names them, do not directly call `input(...)` as implementation logic, and do not paste implementation code from `{source}`.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn.",
            target = self.target,
            source = self.source_path,
            module = self.module_name
        )
    }

    pub(crate) fn tool_schema_description(&self) -> String {
        format!(
            "Complete final test module contents for `{target}`. Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions. If public command examples for `{source}` are part of the current request/spec, cover them with subprocess-style argv tests that assert return code and stdout/stderr, pass a finite `timeout=` to every generated `subprocess.run(...)` child command, capture stdout/stderr before asserting `CompletedProcess.stdout` or `.stderr`, include captured stdout/stderr diagnostics in every subprocess returncode assertion failure message, import every module used by module-qualified helper references such as `sys.executable`, and when parent-side UTF-8 text decoding is used pass explicit child UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`. Optional launch block may be `if __name__ == \"__main__\": unittest.main()`. `{source}` is the production source under test; do not send production source code, top-level production function definitions, `def main()`, requirement ids as Python class bases, over-specific exception classes/messages not named by the current contract, or direct implementation `input(...)` calls for this test-target turn.",
            target = self.target,
            module = self.module_name,
            source = self.source_path
        )
    }

    pub(crate) fn metadata_json(&self) -> Value {
        json!({
            "kind": "python_test_module_content_shape",
            "target": self.target,
            "source_path": self.source_path,
            "module_name": self.module_name,
            "required_positive_shape": [
                "import unittest",
                format!("import {} or from {} import <public functions>", self.module_name, self.module_name),
                "class Test*(unittest.TestCase)",
                "def test_<behavior>(self)",
                "assert requested behavior by calling the production module or imported public functions",
                "cover prompt/spec public command examples with subprocess argv tests when present",
                "subprocess.run child commands include finite timeout",
                "CompletedProcess stdout/stderr assertions capture the asserted stream",
                "subprocess returncode assertions include captured stdout/stderr failure diagnostics",
                "module-qualified helper references import the referenced module",
                "parent UTF-8 subprocess decoding includes explicit child UTF-8 output authority"
            ],
            "allowed_launch_block": "if __name__ == \"__main__\": unittest.main()",
            "forbidden_shape": [
                "production function definitions",
                "def main()",
                "requirement id symbols used as Test* class bases",
                "over-specific exception classes or localized exception messages not named by the current contract",
                "input(...)",
                format!("pasted implementation code from {}", self.source_path)
            ]
        })
    }
}

pub(crate) fn python_source_content_shape_metadata(target: &str) -> Value {
    json!({
        "kind": "python_source_executable_content_shape",
        "target": target.replace('\\', "/"),
        "required_positive_shape": [
            "effective Python module text",
            "real newline-separated source structure",
            "syntax that can be parsed before semantic verification"
        ],
        "forbidden_shape": [
            "quote-wrapped whole-file source string",
            "dominant literal \\\\n escape sequences instead of real newlines",
            "JSON/Python-escaped serialized source snapshot",
            "test module payload such as unittest/pytest tests"
        ]
    })
}

pub(crate) fn python_source_positive_shape_guidance(target: &str) -> String {
    format!(
        "Required positive Python source shape for `{}`: submit effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification. Forbidden shape: do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, or a unittest/pytest test module payload. Do not send tests, Markdown, or a different deliverable for this source-target turn.",
        target.replace('\\', "/")
    )
}

pub(crate) fn python_source_prompt_contract(target: &str) -> String {
    let target = target.replace('\\', "/");
    format!(
        "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- The `content` must be Python source code for `{target}` only.\n- Required positive Python source shape: submit effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification.\n- Forbidden shape: do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, or a unittest/pytest test module payload.\n- Do not write tests, Markdown, or a different deliverable in this source-target turn.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
    )
}

pub(crate) fn python_source_tool_schema_description(target: &str) -> String {
    format!(
        "Complete final Python source contents for `{}`. Required positive shape: effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification. Do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, unittest/pytest test module payloads, tests, Markdown, or a different deliverable.",
        target.replace('\\', "/")
    )
}

pub(crate) fn text_artifact_content_shape_metadata(target: &str) -> Value {
    json!({
        "kind": "text_artifact_readable_content_shape",
        "target": target.replace('\\', "/"),
        "required_positive_shape": [
            "effective readable text artifact",
            "real newline-separated document structure",
            "Markdown/list/table/fence syntax appears as actual text, not escaped JSON string data"
        ],
        "forbidden_shape": [
            "quote-wrapped whole-document string",
            "dominant literal \\\\n escape sequences instead of real newlines",
            "JSON/Python-escaped serialized Markdown or text snapshot"
        ]
    })
}

pub(crate) fn text_artifact_positive_shape_guidance(target: &str) -> String {
    format!(
        "Required positive text artifact shape for `{}`: submit effective Markdown/text content with real newline-separated document structure. Markdown headings, lists, tables, and fenced code blocks must appear as actual text, not as JSON/Python-escaped string data. Forbidden shape: do not send a quote-wrapped whole-document string, JSON/Python-escaped serialized Markdown/text, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn text_artifact_prompt_contract(target: &str) -> String {
    let target = target.replace('\\', "/");
    format!(
        "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- The `content` must be Markdown/documentation text for `{target}` only.\n- Required positive text artifact shape: submit effective readable Markdown/text with real newline-separated document structure; headings, lists, tables, and fenced code blocks must be actual text.\n- Forbidden shape: do not send a quote-wrapped whole-document string, JSON/Python-escaped serialized Markdown/text, or content dominated by literal `\\n` escape sequences instead of real newlines.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
    )
}

pub(crate) fn text_artifact_tool_schema_description(target: &str) -> String {
    format!(
        "Complete final Markdown/text contents for `{}`. Required positive shape: effective readable document text with real newline-separated structure; headings, lists, tables, and fenced code blocks must be actual text. Do not send a quote-wrapped whole-document string, JSON/Python-escaped serialized Markdown/text, or content dominated by literal `\\n` escape sequences instead of real newlines.",
        target.replace('\\', "/")
    )
}

pub(crate) fn text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes() -> bool {
    let good = "# Component Design\n\n## Tests\n\n- `test_component.py` covers public behavior.\n";
    let escaped = "\"# Component Design\\n\\n## Tests\\n\\n- `test_component.py` covers public behavior.\\n\\n```\\npython -m unittest\\n```\\n\"";
    text_artifact_content_has_readable_shape("docs/component-design.md", good)
        && !text_artifact_content_has_readable_shape("docs/component-design.md", escaped)
        && !text_artifact_content_is_serialized_string_snapshot(good)
        && text_artifact_content_is_serialized_string_snapshot(escaped)
}

pub(crate) fn python_source_executable_shape_rejects_escaped_whole_file_fixture_passes() -> bool {
    let good = r#"
import math

def square(value):
    return value * value

if __name__ == "__main__":
    print(square(3))
"#;
    let escaped = "\"import math\\n\\ndef square(value):\\n    return value * value\\n\\nif __name__ == \\\"__main__\\\":\\n    print(square(3))\\n\"";
    python_source_content_has_executable_shape("component.py", good)
        && !python_source_content_has_executable_shape("component.py", escaped)
        && python_artifact_content_has_executable_shape("component.py", good)
        && !python_artifact_content_has_executable_shape("component.py", escaped)
}

pub(crate) fn python_source_executable_shape_rejects_test_module_payload_fixture_passes() -> bool {
    let good = r#"
def add(left, right):
    return left + right

if __name__ == "__main__":
    print(add(2, 3))
"#;
    let test_payload = r#"
import unittest
import component

class TestComponent(unittest.TestCase):
    def test_add(self):
        self.assertEqual(component.add(2, 3), 5)

if __name__ == "__main__":
    unittest.main()
"#;
    let mentions_tests_only = r#"
def describe():
    return "tests should cover this module"
"#;
    python_source_content_has_executable_shape("component.py", good)
        && python_artifact_content_has_executable_shape("component.py", good)
        && !python_source_content_has_executable_shape("component.py", test_payload)
        && !python_artifact_content_has_executable_shape("component.py", test_payload)
        && python_source_content_is_test_module_payload(test_payload)
        && !python_source_content_is_test_module_payload(mentions_tests_only)
        && python_source_content_has_executable_shape("component.py", mentions_tests_only)
}

pub(crate) fn python_source_executable_shape_rejects_markdown_payload_fixture_passes() -> bool {
    let good = r#"
def calculate(left, operator, right):
    if operator == "+":
        return left + right
    raise ValueError("unsupported operator")

if __name__ == "__main__":
    print(calculate(1, "+", 2))
"#;
    let markdown_payload = r#"# component.py

## 要件

- CLI エントリポイント: `if __name__ == "__main__"`
- 演算: 加算

```bash
python component.py 1 + 2
```
"#;
    python_source_content_has_executable_shape("component.py", good)
        && !python_source_content_has_executable_shape("component.py", markdown_payload)
        && !python_artifact_content_has_executable_shape("component.py", markdown_payload)
        && python_source_content_is_markdown_or_prose_payload(markdown_payload)
}

pub(crate) fn python_source_executable_shape_rejects_raw_prose_line_fixture_passes() -> bool {
    let good = r#"
def calculate(left, operator, right):
    if operator == "+":
        return left + right
    raise ValueError("unsupported operator")

if __name__ == "__main__":
    print(calculate(1, "+", 2))
"#;
    let raw_prose_between_code = r#"
# 電卓 CLI アプリケーション

四則演算 (+, -, *, /) をサポートする Python CLI 電卓。

def calculate(left, operator, right):
    if operator == "+":
        return left + right
    raise ValueError("unsupported operator")
"#;
    let commented_japanese = r#"
# 電卓 CLI アプリケーション
# 四則演算 (+, -, *, /) をサポートする Python CLI 電卓。

def calculate(left, operator, right):
    if operator == "+":
        return left + right
    raise ValueError("unsupported operator")
"#;
    python_source_content_has_executable_shape("component.py", good)
        && !python_source_content_has_executable_shape("component.py", raw_prose_between_code)
        && !python_artifact_content_has_executable_shape("component.py", raw_prose_between_code)
        && python_source_content_has_executable_shape("component.py", commented_japanese)
        && python_artifact_content_has_executable_shape("component.py", commented_japanese)
        && !python_source_content_has_executable_shape(
            "component.py",
            r#"
def calculate(left, operator, right):
    if operator == "+":
        return left + right

Usage:
    python component.py 1 + 2
"#,
        )
}

pub(crate) fn test_target_content_shape_projection_is_positive_and_forbidden() -> bool {
    let Some(contract) = python_source_for_test_target("test_component.py") else {
        return false;
    };
    let prompt = contract.prompt_contract();
    let schema = contract.tool_schema_description();
    let guidance = contract.positive_shape_guidance();
    let metadata = contract.metadata_json();
    prompt.contains("Required positive shape")
        && prompt.contains("Test*")
        && prompt.contains("Public command coverage")
        && prompt.contains("capture those streams")
        && prompt.contains("returncode assertion")
        && prompt.contains("module-qualified helper references")
        && prompt.contains("Forbidden shape")
        && schema.contains("Required positive shape")
        && schema.contains("subprocess-style argv tests")
        && schema.contains("timeout=")
        && schema.contains("capture stdout/stderr")
        && schema.contains("returncode assertion")
        && schema.contains("module-qualified helper references")
        && schema.contains("do not send production source code")
        && schema.contains("requirement ids as Python class bases")
        && schema.contains("over-specific exception classes")
        && guidance.contains("def test_...")
        && guidance.contains("public command examples")
        && guidance.contains("do not define `main()`")
        && guidance.contains("FILE_ID")
        && guidance.contains("localized exception messages")
        && metadata["kind"] == "python_test_module_content_shape"
        && metadata["module_name"] == "component"
        && metadata["required_positive_shape"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_str().is_some_and(|value| {
                        value.contains("returncode assertions include captured")
                    })
                }) && items.iter().any(|item| {
                    item.as_str().is_some_and(|value| {
                        value.contains("module-qualified helper references import")
                    })
                })
            })
        && metadata["forbidden_shape"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|value| value.contains("requirement id symbols"))
            }) && items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|value| value.contains("over-specific exception"))
            })
        })
}

pub(crate) fn test_target_subprocess_returncode_assertion_diagnostics_fixture_passes() -> bool {
    let good = r#"
import os
import subprocess
import sys
import unittest
import component

class TestComponentCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", "component.py"],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
        self.assertIn("Error", result.stderr)
"#;
    let bad = r#"
import os
import subprocess
import sys
import unittest
import component

class TestComponentCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", "component.py"],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("Error", result.stderr)
"#;
    python_test_module_content_has_executable_shape("test_component.py", good)
        && !python_test_module_content_has_executable_shape("test_component.py", bad)
        && test_target_has_opaque_subprocess_returncode_assertion(bad)
        && !test_target_has_opaque_subprocess_returncode_assertion(good)
}

pub(crate) fn test_target_module_qualified_reference_import_fixture_passes() -> bool {
    let good = r#"
import os
import subprocess
import sys
import unittest
import component

class TestComponentCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "component.py")],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    let missing_sys = r#"
import os
import subprocess
import unittest
import component

class TestComponentCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "component.py")],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    let missing_os = r#"
import subprocess
import sys
import unittest
import component

class TestComponentCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "component.py")],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    python_test_module_content_has_executable_shape("test_component.py", good)
        && !python_test_module_content_has_executable_shape("test_component.py", missing_sys)
        && !python_test_module_content_has_executable_shape("test_component.py", missing_os)
        && !test_target_has_missing_module_import_for_qualified_reference(good)
        && test_target_has_missing_module_import_for_qualified_reference(missing_sys)
        && test_target_has_missing_module_import_for_qualified_reference(missing_os)
}

pub(crate) fn test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes() -> bool {
    let good = r#"
import unittest
import component

class TestComponent(unittest.TestCase):
    def test_add(self):
        self.assertEqual(component.add(2, 3), 5)
"#;
    let wrapped = "\"import unittest\\nimport component\\nclass TestComponent(unittest.TestCase):\\n    def test_add(self):\\n        self.assertEqual(component.add(2, 3), 5)\\n\"";
    python_test_module_content_has_executable_shape("test_component.py", good)
        && !python_test_module_content_has_executable_shape("test_component.py", wrapped)
}

pub(crate) fn test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
-> bool {
    let good = r#"
import unittest
import component

class TestComponent(unittest.TestCase):
    def test_add(self):
        self.assertEqual(component.add(2, 3), 5)
"#;
    let bad = r#"
import unittest
from component import add

class TestComponent(FILE_ID, API_ID, BEH_ID):
    def test_add(self):
        self.assertEqual(add(2, 3), 5)
"#;
    let bad_markers = detected_test_target_forbidden_content_markers(bad);
    python_test_module_content_has_executable_shape("test_component.py", good)
        && !python_test_module_content_has_executable_shape("test_component.py", bad)
        && bad_markers
            .iter()
            .any(|marker| marker == "class Test* missing unittest.TestCase base")
}

fn snake_to_pascal(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<String>()
}
