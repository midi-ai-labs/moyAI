use std::collections::BTreeSet;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LanguageFamily {
    Python,
    Code,
    Text,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactRole {
    Source,
    Test,
    Document,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactTargetSpec {
    pub(crate) normalized_target: String,
    pub(crate) language: LanguageFamily,
    pub(crate) role: ArtifactRole,
    pub(crate) source_path: Option<String>,
    pub(crate) module_name: Option<String>,
    pub(crate) class_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LanguageArtifactShapeContract {
    pub(crate) target: String,
    pub(crate) source_path: String,
    pub(crate) module_name: String,
    pub(crate) class_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LanguageSourceArtifactShapeContract {
    pub(crate) target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageOutputStreamMismatch {
    pub(crate) stream: String,
    pub(crate) expected_substring: String,
    pub(crate) observed_value: String,
    pub(crate) observed_output: String,
    pub(crate) assertion_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageGeneratedTestLoggingContractOverreach {
    pub(crate) logger_name: Option<String>,
    pub(crate) level: Option<String>,
    pub(crate) assertion_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageParseDefect {
    pub(crate) detail: String,
    pub(crate) path: Option<String>,
    pub(crate) line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageNameResolutionDefect {
    pub(crate) missing_name: String,
    pub(crate) suggested_name: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageLocalBindingContradiction {
    pub(crate) test_target: String,
    pub(crate) label: String,
    pub(crate) identifier: String,
    pub(crate) assignment_line: String,
    pub(crate) assertion_line: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicMissingMethodAttribute {
    pub(crate) attribute: String,
    pub(crate) call_site: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicClassOrEnumMissingMemberDetail {
    pub(crate) member: String,
    pub(crate) suggested_existing_member: Option<String>,
    pub(crate) expected_value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicConstructorSignatureMismatch {
    pub(crate) constructor: String,
    pub(crate) detail: String,
    pub(crate) unexpected_keyword: Option<String>,
    pub(crate) call_site: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicCallableSignatureMismatch {
    pub(crate) callable: String,
    pub(crate) detail: String,
    pub(crate) missing_arguments: Vec<String>,
    pub(crate) call_site: Option<String>,
    pub(crate) source_target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicConstructorBodyExceptionObservation {
    pub(crate) constructor_call_site: String,
    pub(crate) source_initializer_call: Option<String>,
    pub(crate) source_failure_site: Option<String>,
    pub(crate) actual_exception: String,
    pub(crate) sibling_constructor_obligations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicExceptionMismatch {
    pub(crate) actual_exception: String,
    pub(crate) expected_exception: Option<String>,
    pub(crate) call_site: Option<String>,
    pub(crate) source_site: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguagePublicExpectedExceptionNotRaised {
    pub(crate) expected_exception: String,
    pub(crate) call_site: Option<String>,
}

pub(crate) fn classify_artifact_target(target: &str) -> ArtifactTargetSpec {
    let normalized_target = target.replace('\\', "/");
    let lower = normalized_target.to_ascii_lowercase();

    if let Some((source_path, module_name, class_name)) =
        python_test_target_projection(&normalized_target)
    {
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Python,
            role: ArtifactRole::Test,
            source_path: Some(source_path),
            module_name: Some(module_name),
            class_name: Some(class_name),
        };
    }

    if lower.ends_with(".py") {
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Python,
            role: ArtifactRole::Source,
            source_path: None,
            module_name: None,
            class_name: None,
        };
    }

    if code_like_test_target(&lower) {
        let source_path = code_like_test_source_projection(&normalized_target);
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Code,
            role: ArtifactRole::Test,
            source_path,
            module_name: None,
            class_name: None,
        };
    }

    if lower.ends_with(".contract") {
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Text,
            role: ArtifactRole::Test,
            source_path: None,
            module_name: None,
            class_name: None,
        };
    }

    if code_like_source_target(&lower) {
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Code,
            role: ArtifactRole::Source,
            source_path: None,
            module_name: None,
            class_name: None,
        };
    }

    if matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("md" | "markdown" | "txt" | "rst")
    ) {
        return ArtifactTargetSpec {
            normalized_target,
            language: LanguageFamily::Text,
            role: ArtifactRole::Document,
            source_path: None,
            module_name: None,
            class_name: None,
        };
    }

    ArtifactTargetSpec {
        normalized_target,
        language: LanguageFamily::Unknown,
        role: ArtifactRole::Unknown,
        source_path: None,
        module_name: None,
        class_name: None,
    }
}

fn code_like_test_source_projection(target: &str) -> Option<String> {
    let normalized = target.replace('\\', "/");
    let (dir, file_name) = normalized
        .rsplit_once('/')
        .map_or(("", normalized.as_str()), |(dir, file)| (dir, file));
    let (stem, ext) = file_name.rsplit_once('.')?;
    let source_stem = stem
        .strip_suffix(".spec")
        .or_else(|| stem.strip_suffix(".test"))
        .or_else(|| stem.strip_suffix("_spec"))
        .or_else(|| stem.strip_suffix("_test"))
        .or_else(|| stem.strip_suffix("-spec"))
        .or_else(|| stem.strip_suffix("-test"))
        .or_else(|| stem.strip_prefix("test_"))?;
    if source_stem.is_empty() {
        return None;
    }
    let source_file = format!("{source_stem}.{ext}");
    if dir.is_empty() {
        return Some(source_file);
    }
    let mut segments = dir.split('/').collect::<Vec<_>>();
    if segments.first().is_some_and(|segment| *segment == "tests") {
        segments[0] = "src";
        return Some(format!("{}/{}", segments.join("/"), source_file));
    }
    if segments
        .last()
        .is_some_and(|segment| *segment == "__tests__")
    {
        segments.pop();
        if segments.is_empty() {
            return Some(source_file);
        }
        return Some(format!("{}/{}", segments.join("/"), source_file));
    }
    Some(format!("{dir}/{source_file}"))
}

pub(crate) fn code_like_test_source_projection_fixture_passes() -> bool {
    let root_test = classify_artifact_target("workflow.spec.ts");
    let nested_test = classify_artifact_target("tests/workflow.spec.ts");
    let colocated_test = classify_artifact_target("src/__tests__/workflow.test.ts");
    root_test.language == LanguageFamily::Code
        && root_test.role == ArtifactRole::Test
        && root_test.source_path.as_deref() == Some("workflow.ts")
        && nested_test.language == LanguageFamily::Code
        && nested_test.role == ArtifactRole::Test
        && nested_test.source_path.as_deref() == Some("src/workflow.ts")
        && colocated_test.language == LanguageFamily::Code
        && colocated_test.role == ArtifactRole::Test
        && colocated_test.source_path.as_deref() == Some("src/workflow.ts")
}

pub(crate) fn language_test_artifact_shape_contract(
    target: &str,
) -> Option<LanguageArtifactShapeContract> {
    let spec = classify_artifact_target(target);
    if spec.language != LanguageFamily::Python || spec.role != ArtifactRole::Test {
        return None;
    }
    Some(LanguageArtifactShapeContract {
        target: spec.normalized_target,
        source_path: spec.source_path?,
        module_name: spec.module_name?,
        class_name: spec.class_name?,
    })
}

pub(crate) fn language_source_artifact_shape_contract(
    target: &str,
) -> Option<LanguageSourceArtifactShapeContract> {
    let spec = classify_artifact_target(target);
    if spec.language != LanguageFamily::Python || spec.role != ArtifactRole::Source {
        return None;
    }
    Some(LanguageSourceArtifactShapeContract {
        target: spec.normalized_target,
    })
}

pub(crate) fn python_exception_assertion_summary_evidence(summary: &str) -> bool {
    summary.to_ascii_lowercase().contains("assertraisesregex")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VerificationContractPromptProjection {
    pub call_sites: Vec<String>,
    pub has_argument_order_evidence: bool,
    pub has_missing_surface_evidence: bool,
    pub has_output_stream_evidence: bool,
    pub has_exception_or_error_evidence: bool,
    pub has_python_exception_assertion: bool,
}

pub(crate) fn verification_contract_prompt_projection_from_summary(
    summary: Option<&str>,
) -> VerificationContractPromptProjection {
    let Some(summary) = summary else {
        return VerificationContractPromptProjection::default();
    };
    let lower = summary.to_ascii_lowercase();
    VerificationContractPromptProjection {
        call_sites: extract_verification_contract_call_sites(summary),
        has_argument_order_evidence: lower.contains("unsupported operation:")
            || lower.contains("unsupported operator:")
            || lower.contains("unsupported unary operator:")
            || summary.contains("未対応の演算子"),
        has_missing_surface_evidence: lower.contains("importerror")
            || lower.contains("cannot import name")
            || lower.contains("attributeerror")
            || lower.contains("is not a function")
            || lower.contains("not a function")
            || lower.contains("undefined")
            || lower.contains("no method named"),
        has_output_stream_evidence: lower.contains("stdout") || lower.contains("stderr"),
        has_exception_or_error_evidence: lower.contains("exception")
            || lower.contains("error")
            || lower.contains("throws")
            || lower.contains("panic"),
        has_python_exception_assertion: python_exception_assertion_summary_evidence(summary),
    }
}

fn extract_verification_contract_call_sites(summary: &str) -> Vec<String> {
    let mut call_sites = Vec::new();
    for raw in summary.split(['\n', '|']) {
        let trimmed = raw.trim();
        if !looks_like_contract_call_site(trimmed) {
            continue;
        }
        let normalized = crate::tool::truncate::clip_text_with_ellipsis(
            &trimmed.split_whitespace().collect::<Vec<_>>().join(" "),
            180,
        );
        if !call_sites.iter().any(|existing| existing == &normalized) {
            call_sites.push(normalized);
        }
        if call_sites.len() >= 4 {
            break;
        }
    }
    call_sites
}

fn looks_like_contract_call_site(line: &str) -> bool {
    if line.is_empty()
        || line.starts_with("File \"")
        || line.starts_with("Traceback ")
        || line.starts_with("FAIL: ")
        || line.starts_with("ERROR: ")
        || line.starts_with("FAILED ")
        || line.starts_with("Ran ")
        || line == "----------------------------------------------------------------------"
    {
        return false;
    }
    if !(line.contains('(') && line.contains(')')) {
        return false;
    }

    let lower = line.to_ascii_lowercase();
    lower.contains("assert")
        || lower.contains("expect(")
        || lower.contains("subprocess.run")
        || lower.contains("self._run")
        || lower.contains("output =")
        || lower.contains("result =")
}

pub(crate) fn language_source_artifact_content_has_executable_shape(
    target: &str,
    content: &str,
) -> bool {
    if language_source_artifact_shape_contract(target).is_none() {
        return true;
    }
    !source_artifact_content_is_escaped_whole_file_string(content)
        && !source_artifact_content_is_test_module_payload(content)
        && !source_artifact_content_is_markdown_or_prose_payload(content)
        && !source_artifact_content_has_raw_prose_line(content)
        && !source_artifact_content_has_duplicate_executable_entrypoint(content)
        && source_artifact_content_has_code_shape(content)
}

pub(crate) fn language_source_artifact_forbidden_content_markers(
    target: &str,
    content: &str,
) -> Vec<String> {
    let Some(contract) = language_source_artifact_shape_contract(target) else {
        return Vec::new();
    };
    let mut markers = Vec::new();
    if source_artifact_content_is_escaped_whole_file_string(content) {
        markers.push("quote-wrapped or escaped whole-file source string".to_string());
    }
    if source_artifact_content_is_test_module_payload(content) {
        markers.push("unittest/pytest test module payload".to_string());
    }
    if source_artifact_content_is_markdown_or_prose_payload(content) {
        markers.push("Markdown/prose payload for Python source target".to_string());
    }
    if source_artifact_content_has_raw_prose_line(content) {
        markers.push("raw prose line inside Python source".to_string());
    }
    if source_artifact_content_has_duplicate_executable_entrypoint(content) {
        markers.push("multiple executable entrypoint guards".to_string());
    }
    if markers.is_empty() && !source_artifact_content_has_code_shape(content) {
        markers.push(format!(
            "no executable Python source shape for {}",
            contract.target
        ));
    }
    markers
}

pub(crate) fn language_source_artifact_content_is_escaped_whole_file_string(
    target: &str,
    content: &str,
) -> bool {
    language_source_artifact_shape_contract(target).is_some()
        && source_artifact_content_is_escaped_whole_file_string(content)
}

pub(crate) fn language_test_artifact_content_has_executable_shape(
    target: &str,
    content: &str,
) -> bool {
    let Some(contract) = language_test_artifact_shape_contract(target) else {
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
        && !test_target_has_recursive_runner_self_invocation(content, &contract)
}

pub(crate) fn language_test_artifact_forbidden_content_markers(
    target: &str,
    content: &str,
) -> Vec<String> {
    let mut markers = BTreeSet::new();
    let executable = python_code_without_strings_or_comments(content);
    if let Some(contract) = language_test_artifact_shape_contract(target) {
        if test_target_has_recursive_runner_self_invocation(content, &contract) {
            markers.insert("recursive test-runner self-invocation".to_string());
        }
    }
    if contains_direct_input_call(content) {
        markers.insert("input(".to_string());
    }
    if test_target_has_invalid_test_class_base(&executable) {
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

impl LanguageArtifactShapeContract {
    pub(crate) fn positive_shape_guidance(&self) -> String {
        format!(
            " Required positive test-module shape for `{target}`: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert behavior by calling `{module}.<public_function>(...)` or imported public functions. If the current request/spec includes public command examples for `{source}`, cover them with subprocess-style argv tests that assert return code and observable stdout/stderr; every generated `subprocess.run(...)` child command must pass a finite `timeout=`, any assertion that reads `CompletedProcess.stdout` or `.stderr` must use `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE`, every returncode assertion for a subprocess result must include captured stdout/stderr diagnostics in its failure message, module-qualified helper references such as `sys.executable` must import that module, parent-side UTF-8 text decoding such as `encoding=\"utf-8\"` must give the child explicit UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`, and generated tests must not recursively invoke the current test artifact through any test runner. Optional launch block is allowed only as `if __name__ == \"__main__\": unittest.main()`. Forbidden shape: do not define production functions at module top level, do not define `main()`, do not use requirement ids such as `FILE_ID` / `API_ID` / `BEH_ID` as Python class bases, do not over-specify concrete exception classes or localized exception messages unless the current contract names them, do not directly call `input(...)` as implementation logic, do not recursively invoke the current test artifact through a test runner, and do not paste implementation code from `{source}`.",
            target = self.target,
            module = self.module_name,
            source = self.source_path
        )
    }

    pub(crate) fn prompt_contract(&self) -> String {
        format!(
            "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- `{source}` is the inferred production source under test; do not rewrite `{source}` in this turn.\n- The `content` must be a complete test module for `{target}` only.\n- Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions.\n- Public command coverage: when the prompt/spec includes public command examples for `{source}`, add subprocess-style tests that execute the requested argv forms and assert return code plus stdout/stderr behavior. Every generated `subprocess.run(...)` child command must pass a finite `timeout=` so verification cannot block indefinitely, any test that reads `CompletedProcess.stdout` or `.stderr` must capture those streams with `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE`, every returncode assertion for a subprocess result must include captured stdout/stderr diagnostics in its failure message, module-qualified helper references such as `sys.executable` must import that module, parent-side UTF-8 text decoding such as `encoding=\"utf-8\"` must pass explicit child UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`, and generated tests must not recursively invoke `{target}` or its test identity through any test runner.\n- Allowed launch block: `if __name__ == \"__main__\": unittest.main()`.\n- Forbidden shape: do not define production functions at module top level, do not define `main()`, do not use requirement ids such as `FILE_ID` / `API_ID` / `BEH_ID` as Python class bases, do not over-specify concrete exception classes or localized exception messages unless the current contract names them, do not directly call `input(...)` as implementation logic, do not recursively invoke the current test artifact through a test runner, and do not paste implementation code from `{source}`.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn.",
            target = self.target,
            source = self.source_path,
            module = self.module_name
        )
    }

    pub(crate) fn tool_schema_description(&self) -> String {
        format!(
            "Complete final test module contents for `{target}`. Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define one or more `Test*` classes extending `unittest.TestCase`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions. Generated-test recovery scaffold: the active patch must be a single `*** Add File: {target}` or `*** Update File: {target}` operation whose content starts with `+import unittest`, imports `{module}`, defines `+class {class_name}(unittest.TestCase):`, and includes `+    def test_<requested_behavior>(self):` with assertions that call `{module}`; do not leave placeholder assertions or assert only that the module imported. If public command examples for `{source}` are part of the current request/spec, cover them with subprocess-style argv tests that assert return code and stdout/stderr, pass a finite `timeout=` to every generated `subprocess.run(...)` child command, capture stdout/stderr before asserting `CompletedProcess.stdout` or `.stderr`, include captured stdout/stderr diagnostics in every subprocess returncode assertion failure message, import every module used by module-qualified helper references such as `sys.executable`, when parent-side UTF-8 text decoding is used pass explicit child UTF-8 output authority with `PYTHONUTF8=1` plus `PYTHONIOENCODING=utf-8` or `python -X utf8`, and do not recursively invoke `{target}` or its test identity through any test runner. Optional launch block may be `if __name__ == \"__main__\": unittest.main()`. `{source}` is the production source under test; do not send production source code, top-level production function definitions, `def main()`, requirement ids as Python class bases, over-specific exception classes/messages not named by the current contract, direct implementation `input(...)` calls, or recursive test-runner self-invocation for this test-target turn.",
            target = self.target,
            module = self.module_name,
            source = self.source_path,
            class_name = self.class_name
        )
    }

    pub(crate) fn apply_patch_recovery_scaffold(&self) -> String {
        format!(
            "Positive generated-test apply_patch scaffold for `{target}`:\n- Use one active-target operation only: `*** Add File: {target}` when missing, or `*** Update File: {target}` when present.\n- The patch content must start with `+import unittest`, import `{module}`, define `+class {class_name}(unittest.TestCase):`, and define `+    def test_<requested_behavior>(self):`.\n- Assertion lines must call `{module}.<public_function>(...)` or imported public functions according to the current request/spec. Do not leave placeholder assertions, do not assert only that the module imported, and do not paste implementation code from `{source}`.",
            target = self.target,
            module = self.module_name,
            class_name = self.class_name,
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
                "parent UTF-8 subprocess decoding includes explicit child UTF-8 output authority",
                "generated tests do not recursively invoke the current test artifact through any test runner"
            ],
            "allowed_launch_block": "if __name__ == \"__main__\": unittest.main()",
            "forbidden_shape": [
                "production function definitions",
                "def main()",
                "requirement id symbols used as Test* class bases",
                "over-specific exception classes or localized exception messages not named by the current contract",
                "input(...)",
                "recursive test-runner self-invocation",
                format!("pasted implementation code from {}", self.source_path)
            ]
        })
    }
}

impl LanguageSourceArtifactShapeContract {
    pub(crate) fn positive_shape_guidance(&self) -> String {
        format!(
            "Required positive Python source shape for `{}`: submit effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification. Forbidden shape: do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, a unittest/pytest test module payload, multiple executable entrypoint guards, or concatenated module copies. Do not send tests, Markdown, or a different deliverable for this source-target turn.",
            self.target
        )
    }

    pub(crate) fn prompt_contract(&self) -> String {
        format!(
            "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- The `content` must be Python source code for `{target}` only.\n- Required positive Python source shape: submit effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification.\n- Forbidden shape: do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, a unittest/pytest test module payload, multiple executable entrypoint guards, or concatenated module copies.\n- Do not write tests, Markdown, or a different deliverable in this source-target turn.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn.",
            target = self.target
        )
    }

    pub(crate) fn tool_schema_description(&self) -> String {
        format!(
            "Complete final Python source contents for `{}`. Required positive shape: effective Python module text with real newline-separated source structure, imports/functions/classes/CLI entrypoint as required by the current task, and syntax that can be parsed before semantic verification. Do not send a quote-wrapped whole-file source string, JSON/Python-escaped serialized source, content dominated by literal `\\n` escape sequences instead of real newlines, unittest/pytest test module payloads, multiple executable entrypoint guards, concatenated module copies, tests, Markdown, or a different deliverable.",
            self.target
        )
    }

    pub(crate) fn metadata_json(&self) -> Value {
        json!({
            "kind": "python_source_executable_content_shape",
            "target": self.target,
            "required_positive_shape": [
                "effective Python module text",
                "real newline-separated source structure",
                "syntax that can be parsed before semantic verification"
            ],
            "forbidden_shape": [
                "quote-wrapped whole-file source string",
                "dominant literal \\\\n escape sequences instead of real newlines",
                "JSON/Python-escaped serialized source snapshot",
                "test module payload such as unittest/pytest tests",
                "multiple executable entrypoint guards or concatenated module copies"
            ]
        })
    }
}

pub(crate) fn language_source_line_has_code_shape(language: LanguageFamily, line: &str) -> bool {
    match language {
        LanguageFamily::Python => python_source_line_has_code_shape(line),
        LanguageFamily::Code => generic_code_source_line_has_code_shape(line),
        LanguageFamily::Text | LanguageFamily::Unknown => false,
    }
}

pub(crate) fn language_executable_surface_without_literals(
    language: LanguageFamily,
    content: &str,
) -> String {
    match language {
        LanguageFamily::Python => python_code_without_strings_or_comments(content),
        LanguageFamily::Code | LanguageFamily::Text | LanguageFamily::Unknown => {
            content.to_string()
        }
    }
}

pub(crate) fn language_source_line_can_be_executable_source(
    language: LanguageFamily,
    line: &str,
) -> bool {
    match language {
        LanguageFamily::Python => python_source_line_can_be_executable_source(line),
        LanguageFamily::Code => generic_code_source_line_has_code_shape(line),
        LanguageFamily::Text | LanguageFamily::Unknown => false,
    }
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

fn python_source_line_can_be_executable_source(line: &str) -> bool {
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
        || lower.starts_with("and ")
        || lower.starts_with("or ")
        || lower.starts_with("del ")
        || lower.starts_with("global ")
        || lower.starts_with("nonlocal ")
        || lower.starts_with("yield ")
        || lower.starts_with("sys.exit(")
        || lower.starts_with("exit(")
        || lower.starts_with("main(")
        || lower.starts_with("unittest.main(")
        || python_source_line_has_call_expression_shape(line)
        || python_source_line_has_argument_continuation_shape(line)
        || python_source_line_has_boolean_comparison_continuation_shape(line)
        || matches!(line, ")" | "]" | "}" | ")," | "]," | "},")
        || line.starts_with('.')
        || line.starts_with(')')
        || line.starts_with(']')
        || line.starts_with('}')
}

fn python_source_line_has_call_expression_shape(line: &str) -> bool {
    let Some(paren_index) = line.find('(') else {
        return false;
    };
    let callee = line[..paren_index].trim();
    if callee.is_empty() || callee.contains(char::is_whitespace) {
        return false;
    }
    callee.split('.').all(|part| {
        let mut chars = part.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        (first == '_' || first.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    })
}

fn python_source_line_has_argument_continuation_shape(line: &str) -> bool {
    line.ends_with(',')
        && line.contains(',')
        && line.chars().any(|ch| {
            ch.is_ascii_digit()
                || matches!(
                    ch,
                    '.' | '_' | '+' | '-' | '*' | '/' | '[' | ']' | '(' | ')'
                )
        })
}

fn python_source_line_has_boolean_comparison_continuation_shape(line: &str) -> bool {
    if line.contains('`') {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let has_boolean_operator = lower.contains(" and ")
        || lower.contains(" or ")
        || lower.ends_with(" and")
        || lower.ends_with(" or")
        || lower.contains(" and\\")
        || lower.contains(" or\\");
    let has_comparison = ["<=", ">=", "==", "!=", "<", ">"]
        .iter()
        .any(|op| lower.contains(op));
    let has_identifier = lower
        .chars()
        .any(|ch| ch == '_' || ch.is_ascii_alphabetic());
    has_boolean_operator && has_comparison && has_identifier
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

fn source_artifact_content_is_escaped_whole_file_string(content: &str) -> bool {
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

fn source_artifact_content_is_test_module_payload(content: &str) -> bool {
    let executable = language_executable_surface_without_literals(LanguageFamily::Python, content);
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

fn source_artifact_content_is_markdown_or_prose_payload(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let code_shape = source_artifact_content_has_code_shape(trimmed);
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
                && !language_source_line_has_code_shape(LanguageFamily::Python, line)
        })
        .count();
    (has_markdown_structure && !code_shape) || (prose_lines >= 2 && !code_shape)
}

fn source_artifact_content_has_raw_prose_line(content: &str) -> bool {
    let executable = language_executable_surface_without_literals(LanguageFamily::Python, content);
    executable.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty()
            && !language_source_line_can_be_executable_source(LanguageFamily::Python, trimmed)
            && source_artifact_line_looks_like_prose(trimmed)
    })
}

fn source_artifact_content_has_duplicate_executable_entrypoint(content: &str) -> bool {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start().to_ascii_lowercase();
            trimmed.starts_with("if ")
                && trimmed.contains("__name__")
                && trimmed.contains("__main__")
        })
        .take(2)
        .count()
        > 1
}

fn source_artifact_content_has_code_shape(content: &str) -> bool {
    let executable = language_executable_surface_without_literals(LanguageFamily::Python, content);
    executable
        .lines()
        .any(|line| language_source_line_has_code_shape(LanguageFamily::Python, line.trim_start()))
}

fn source_artifact_line_looks_like_prose(line: &str) -> bool {
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

fn test_target_has_recursive_runner_self_invocation(
    content: &str,
    contract: &LanguageArtifactShapeContract,
) -> bool {
    let lower = content.to_ascii_lowercase();
    if !looks_like_test_runner_invocation_surface(&lower) {
        return false;
    }
    let identities = test_artifact_identity_tokens(contract);
    let compact = compact_command_like_content(content);
    identities.iter().any(|identity| {
        recursive_test_runner_invocation_patterns(identity)
            .iter()
            .any(|needle| compact.contains(needle))
    })
}

fn looks_like_test_runner_invocation_surface(lower_content: &str) -> bool {
    [
        "subprocess.run",
        "process::command",
        "std::process::command",
        "child_process",
        "command(",
        "exec(",
        "spawn(",
        "system(",
    ]
    .iter()
    .any(|needle| lower_content.contains(needle))
        && [
            "unittest",
            "pytest",
            "cargo test",
            "npm test",
            "pnpm test",
            "yarn test",
            "jest",
            "vitest",
            "go test",
            "dotnet test",
            "mvn test",
            "gradle test",
        ]
        .iter()
        .any(|needle| lower_content.contains(needle))
}

fn test_artifact_identity_tokens(contract: &LanguageArtifactShapeContract) -> Vec<String> {
    let module_stem = contract
        .target
        .rsplit('/')
        .next()
        .unwrap_or(contract.target.as_str())
        .trim_end_matches(".py");
    let mut identities = BTreeSet::new();
    identities.insert(contract.target.replace('\\', "/"));
    identities.insert(module_stem.to_string());
    identities.insert(contract.module_name.clone());
    identities.into_iter().collect()
}

fn compact_command_like_content(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\'', "\"")
        .to_ascii_lowercase()
}

fn recursive_test_runner_invocation_patterns(identity: &str) -> Vec<String> {
    let identity = identity.to_ascii_lowercase();
    vec![
        format!("\"-m\", \"unittest\", \"{identity}\""),
        format!("\"-m\",\"unittest\",\"{identity}\""),
        format!("\"unittest\", \"{identity}\""),
        format!("\"unittest\",\"{identity}\""),
        format!("-m unittest {identity}"),
        format!("\"pytest\", \"{identity}\""),
        format!("\"pytest\",\"{identity}\""),
        format!("pytest {identity}"),
        format!("\"jest\", \"{identity}\""),
        format!("\"jest\",\"{identity}\""),
        format!("jest {identity}"),
        format!("\"vitest\", \"{identity}\""),
        format!("\"vitest\",\"{identity}\""),
        format!("vitest {identity}"),
        format!("npm test -- {identity}"),
        format!("pnpm test -- {identity}"),
        format!("yarn test {identity}"),
        format!("cargo test {identity}"),
        format!("go test {identity}"),
        format!("dotnet test {identity}"),
        format!("mvn test -dtest={identity}"),
        format!("gradle test --tests {identity}"),
    ]
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

pub(crate) fn test_target_content_shape_projection_is_positive_and_forbidden() -> bool {
    let Some(contract) = language_test_artifact_shape_contract("test_workflow.py") else {
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
        && metadata["module_name"] == "workflow"
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
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", "workflow.py"],
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
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", "workflow.py"],
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
    language_test_artifact_content_has_executable_shape("test_workflow.py", good)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", bad)
        && test_target_has_opaque_subprocess_returncode_assertion(bad)
        && !test_target_has_opaque_subprocess_returncode_assertion(good)
}

pub(crate) fn test_target_module_qualified_reference_import_fixture_passes() -> bool {
    let good = r#"
import os
import subprocess
import sys
import unittest
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "workflow.py")],
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
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "workflow.py")],
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
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_invalid_cli(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", os.path.join(".", "workflow.py")],
            input="bad input\nquit\n",
            text=True,
            encoding="utf-8",
            env={},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    language_test_artifact_content_has_executable_shape("test_workflow.py", good)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", missing_sys)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", missing_os)
        && !test_target_has_missing_module_import_for_qualified_reference(good)
        && test_target_has_missing_module_import_for_qualified_reference(missing_sys)
        && test_target_has_missing_module_import_for_qualified_reference(missing_os)
}

pub(crate) fn test_target_rejects_recursive_runner_self_invocation_fixture_passes() -> bool {
    let good = r#"
import os
import subprocess
import sys
import unittest
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_cli_smoke(self):
        result = subprocess.run(
            [sys.executable, "-X", "utf8", "workflow.py", "2", "+", "3"],
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    let bad = r#"
import os
import subprocess
import sys
import unittest
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_unittest_passes(self):
        result = subprocess.run(
            [sys.executable, "-m", "unittest", "test_workflow", "-v"],
            text=True,
            encoding="utf-8",
            env={**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"},
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    let bad_pytest = r#"
import subprocess
import sys
import unittest
import workflow

class TestWorkflowCli(unittest.TestCase):
    def test_pytest_passes(self):
        result = subprocess.run(
            [sys.executable, "-m", "pytest", "test_workflow.py"],
            text=True,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}")
"#;
    language_test_artifact_content_has_executable_shape("test_workflow.py", good)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", bad)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", bad_pytest)
}

pub(crate) fn test_target_executable_shape_rejects_string_literal_wrapper_fixture_passes() -> bool {
    let good = r#"
import unittest
import workflow

class TestWorkflow(unittest.TestCase):
    def test_add(self):
        self.assertEqual(workflow.add(2, 3), 5)
"#;
    let wrapped = "\"import unittest\\nimport workflow\\nclass TestWorkflow(unittest.TestCase):\\n    def test_add(self):\\n        self.assertEqual(workflow.add(2, 3), 5)\\n\"";
    language_test_artifact_content_has_executable_shape("test_workflow.py", good)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", wrapped)
}

pub(crate) fn test_target_executable_shape_rejects_requirement_id_class_bases_fixture_passes()
-> bool {
    let good = r#"
import unittest
import workflow

class TestWorkflow(unittest.TestCase):
    def test_add(self):
        self.assertEqual(workflow.add(2, 3), 5)
"#;
    let bad = r#"
import unittest
from workflow import add

class TestWorkflow(FILE_ID, API_ID, BEH_ID):
    def test_add(self):
        self.assertEqual(add(2, 3), 5)
"#;
    let bad_markers = language_test_artifact_forbidden_content_markers("test_workflow.py", bad);
    language_test_artifact_content_has_executable_shape("test_workflow.py", good)
        && !language_test_artifact_content_has_executable_shape("test_workflow.py", bad)
        && bad_markers
            .iter()
            .any(|marker| marker == "class Test* missing unittest.TestCase base")
}

fn generic_code_source_line_has_code_shape(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    lower.starts_with("#!")
        || lower.starts_with("import ")
        || lower.starts_with("export ")
        || lower.starts_with("from ")
        || lower.starts_with("use ")
        || lower.starts_with("mod ")
        || lower.starts_with("pub ")
        || lower.starts_with("fn ")
        || lower.starts_with("impl ")
        || lower.starts_with("trait ")
        || lower.starts_with("struct ")
        || lower.starts_with("enum ")
        || lower.starts_with("type ")
        || lower.starts_with("interface ")
        || lower.starts_with("namespace ")
        || lower.starts_with("module ")
        || lower.starts_with("class ")
        || lower.starts_with("function ")
        || lower.starts_with("const ")
        || lower.starts_with("let ")
        || lower.starts_with("var ")
        || lower.starts_with("return ")
        || lower.starts_with("if ")
        || lower.starts_with("for ")
        || lower.starts_with("while ")
        || lower.starts_with("switch ")
        || lower.starts_with("case ")
        || lower.starts_with("func ")
        || lower.starts_with("package ")
        || lower.starts_with("using ")
        || lower.starts_with("public ")
        || lower.starts_with("private ")
        || lower.starts_with("protected ")
        || lower.starts_with("static ")
        || lower.starts_with("async ")
        || lower.starts_with("@")
        || line == "{"
        || line == "}"
        || line == "];"
        || line == "},"
        || line.starts_with('}')
        || line.starts_with(']')
        || line.ends_with(';')
        || line.ends_with('{')
        || line.ends_with('}')
        || line.contains("=>")
        || line.contains("::")
        || line.contains(":=")
        || line.contains("==")
        || line.contains("!=")
        || line.contains("&&")
        || line.contains("||")
        || generic_code_source_line_has_assignment_shape(line)
        || generic_code_source_line_has_config_shape(line)
}

fn generic_code_source_line_has_assignment_shape(line: &str) -> bool {
    if line.starts_with('-') || line.contains('`') {
        return false;
    }
    if line.contains("==") || line.contains("!=") || line.contains("<=") || line.contains(">=") {
        return false;
    }
    let Some((left, right)) = line.split_once('=') else {
        return false;
    };
    language_identifier_like_key(left.trim()) && !right.trim().is_empty()
}

fn generic_code_source_line_has_config_shape(line: &str) -> bool {
    if line.starts_with('[') && line.ends_with(']') && line.len() > 2 {
        return true;
    }
    let Some((left, right)) = line.split_once(':') else {
        return false;
    };
    let left = left.trim().trim_matches('"').trim_matches('\'');
    !right.trim().is_empty() && language_identifier_like_key(left)
}

fn language_identifier_like_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_alphanumeric())
}

pub(crate) fn language_verification_repair_authority_target(target: &str) -> bool {
    if language_verification_runner_byproduct_or_dependency(target) {
        return false;
    }
    let spec = classify_artifact_target(target);
    matches!(
        spec.role,
        ArtifactRole::Source | ArtifactRole::Test | ArtifactRole::Document
    ) || language_source_configuration_target(target)
}

pub(crate) fn language_verification_runner_byproduct_or_dependency(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".pyc")
        || normalized.ends_with(".pyo")
        || normalized.ends_with(".pytest_cache")
        || normalized.split('/').any(|segment| {
            matches!(
                segment,
                ".git"
                    | ".hg"
                    | ".svn"
                    | ".moyai"
                    | ".venv"
                    | "venv"
                    | ".pytest_cache"
                    | ".ruff_cache"
                    | "__pycache__"
                    | "node_modules"
                    | "target"
                    | "build-artifacts"
                    | ".next"
                    | "dist"
                    | "build"
                    | "coverage"
                    | "playwright-report"
                    | "test-results"
            )
        })
}

pub(crate) fn language_source_configuration_target(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    matches!(
        file_name,
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "deno.json"
            | "deno.jsonc"
            | "tsconfig.json"
            | "jsconfig.json"
            | "vite.config.js"
            | "vite.config.ts"
            | "webpack.config.js"
            | "rollup.config.js"
            | "eslint.config.js"
            | "makefile"
            | "dockerfile"
    ) || matches!(
        normalized.rsplit_once('.').map(|(_, ext)| ext),
        Some(
            "json"
                | "jsonc"
                | "toml"
                | "yaml"
                | "yml"
                | "html"
                | "css"
                | "scss"
                | "sass"
                | "vue"
                | "svelte"
        )
    )
}

pub(crate) fn language_failure_label_from_output_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("FAIL: ") {
        return Some(compact_language_failure_label(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("ERROR: ") {
        return Some(compact_language_failure_label(rest));
    }
    if !(trimmed.contains("... FAIL")
        || trimmed.contains("... ERROR")
        || trimmed.contains(" ... FAIL")
        || trimmed.contains(" ... ERROR"))
    {
        return None;
    }
    let label = trimmed.split_whitespace().next().unwrap_or(trimmed);
    (classify_artifact_target(label).role == ArtifactRole::Test).then(|| label.to_string())
}

pub(crate) fn language_failure_labels_from_summary(summary: &str, limit: usize) -> Vec<String> {
    let mut labels = Vec::new();
    for line in summary.lines() {
        let Some(label) = language_failure_label_from_output_line(line) else {
            continue;
        };
        if !labels.iter().any(|existing| existing == &label) {
            labels.push(label);
        }
        if labels.len() >= limit {
            break;
        }
    }
    labels
}

fn compact_language_failure_label(label: &str) -> String {
    label
        .split_whitespace()
        .next()
        .unwrap_or(label)
        .trim_matches(':')
        .to_string()
}

pub(crate) const LANGUAGE_VERIFICATION_COMMAND_PREFIXES: &[&str] = &[
    "bun test",
    "cargo build",
    "cargo check",
    "cargo test",
    "deno test",
    "dotnet test",
    "go test",
    "gradle test",
    "mvn test",
    "node --test",
    "npx jest",
    "npx vitest",
    "npm run test",
    "npm test",
    "pnpm run test",
    "pnpm test",
    "python -x utf8 -m py_compile",
    "python -x utf8 -m unittest",
    "python -m py_compile",
    "python -m unittest",
    "pytest",
    "verify-contract",
    "verify-contract --behavior",
    "verify-generated-test",
    "verify-public-command",
    "vitest",
    "yarn test",
];

const LANGUAGE_TEST_RUNNER_COMMAND_PREFIXES: &[&str] = &[
    "bun test",
    "cargo test",
    "deno test",
    "dotnet test",
    "go test",
    "gradle test",
    "mvn test",
    "node --test",
    "npx jest",
    "npx vitest",
    "npm run test",
    "npm test",
    "pnpm run test",
    "pnpm test",
    "python -x utf8 -m unittest",
    "python -m unittest",
    "pytest",
    "vitest",
    "yarn test",
];

const LANGUAGE_BUILD_CHECK_COMMAND_PREFIXES: &[&str] = &[
    "cargo build",
    "cargo check",
    "python -x utf8 -m py_compile",
    "python -m py_compile",
];

const LANGUAGE_TEXT_IO_COMMAND_TOKENS: &[&str] = &[
    "bun", "cargo", "deno", "dotnet", "go", "gradle", "java", "javac", "mvn", "node", "npm",
    "perl", "php", "pnpm", "py", "pytest", "python", "python3", "ruby", "rustc", "unittest",
    "yarn",
];

const LANGUAGE_RUNTIME_EXECUTION_TOKENS: &[&str] = &[
    "bun", "deno", "java", "node", "perl", "php", "py", "python", "python3", "ruby",
];

const LANGUAGE_TEST_OR_VERIFICATION_TEXT_IO_TOKENS: &[&str] = &[
    "bun", "cargo", "deno", "dotnet", "go", "gradle", "jest", "mvn", "npm", "pnpm", "pytest",
    "test", "tests", "unittest", "vitest", "yarn",
];

const PYTHON_UTF8_BOOTSTRAP_TOKENS: &[&str] = &["py", "pytest", "python", "python3", "unittest"];

pub(crate) fn language_verification_command_evidence(lower: &str) -> bool {
    LANGUAGE_VERIFICATION_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lower.contains(prefix))
        || python_direct_test_script_evidence(lower)
}

pub(crate) fn language_test_runner_evidence(lower: &str) -> bool {
    LANGUAGE_TEST_RUNNER_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lower.contains(prefix))
        || python_direct_test_script_evidence(lower)
}

pub(crate) fn language_build_check_verification_evidence(lower: &str) -> bool {
    LANGUAGE_BUILD_CHECK_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lower.contains(prefix))
}

pub(crate) fn language_command_text_io_surface_evidence(tokens: &[String], lower: &str) -> bool {
    language_verification_command_evidence(lower)
        || tokens
            .iter()
            .any(|token| LANGUAGE_TEXT_IO_COMMAND_TOKENS.contains(&token.as_str()))
}

pub(crate) fn language_command_test_or_verification_io_evidence(
    tokens: &[String],
    lower: &str,
) -> bool {
    language_verification_command_evidence(lower)
        || tokens
            .iter()
            .any(|token| LANGUAGE_TEST_OR_VERIFICATION_TEXT_IO_TOKENS.contains(&token.as_str()))
}

pub(crate) fn language_runtime_execution_io_evidence(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| LANGUAGE_RUNTIME_EXECUTION_TOKENS.contains(&token.as_str()))
}

pub(crate) fn language_command_inherits_utf8_bootstrap(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| PYTHON_UTF8_BOOTSTRAP_TOKENS.contains(&token.as_str()))
}

pub(crate) fn language_python_utf8_correction_applies(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| matches!(token.as_str(), "python" | "python3" | "py" | "pytest"))
}

fn python_direct_test_script_evidence(lower: &str) -> bool {
    (lower.contains("python") || lower.starts_with("py "))
        && (lower.contains("test_")
            || lower.contains("_test.py")
            || lower.contains("/tests/")
            || lower.contains("\\tests\\"))
}

pub(crate) fn normalize_language_verification_command(text: &str) -> String {
    let collapsed = if text.to_ascii_lowercase().starts_with("uv run pytest") {
        text["uv run ".len()..].to_string()
    } else {
        text.to_string()
    };
    normalize_python_module_verification_command(&collapsed)
}

pub(crate) fn looks_like_language_explicit_verification_command(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    LANGUAGE_VERIFICATION_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || lower.starts_with("npx jest")
        || lower.starts_with("npx vitest")
}

pub(crate) fn looks_like_language_direct_shell_verification_command(text: &str) -> bool {
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
        "node" => tokens.iter().skip(1).any(|token| {
            let lower = token.to_ascii_lowercase();
            lower == "--test"
                || lower.ends_with(".js")
                || lower.ends_with(".mjs")
                || lower.ends_with(".cjs")
        }),
        "deno" => tokens
            .get(1)
            .is_some_and(|token| token.eq_ignore_ascii_case("test")),
        "bun" => tokens
            .get(1)
            .is_some_and(|token| token.eq_ignore_ascii_case("test")),
        _ => false,
    }
}

pub(crate) fn language_verification_target_candidates(token: &str) -> Vec<String> {
    let candidate = token.trim();
    if candidate.is_empty() || candidate.starts_with('-') {
        return Vec::new();
    }

    let lower = candidate.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "python" | "python.exe" | "py" | "-m" | "unittest" | "pytest" | "discover"
    ) {
        return Vec::new();
    }

    if candidate.contains('/')
        || candidate.contains('\\')
        || lower.ends_with(".py")
        || classify_artifact_target(candidate).role != ArtifactRole::Unknown
    {
        return vec![candidate.to_string()];
    }

    if !(lower.starts_with("test")
        || lower.contains("integration")
        || lower.contains("spec")
        || lower.contains(".test"))
    {
        return Vec::new();
    }

    let module_path = candidate.replace('.', "/");
    let mut candidates = vec![
        format!("{module_path}.py"),
        format!("{candidate}.py"),
        format!("tests/{module_path}.py"),
        format!("tests/{candidate}.py"),
    ];
    if lower.contains("spec") || lower.contains("test") {
        for ext in ["ts", "tsx", "js", "jsx", "rs", "go"] {
            candidates.push(format!("{candidate}.{ext}"));
            candidates.push(format!("tests/{candidate}.{ext}"));
            candidates.push(format!("src/{candidate}.{ext}"));
        }
    }
    candidates
}

pub(crate) fn language_verification_failure_summary_evidence(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    language_verification_command_evidence(&lower)
        || lower.contains("unittest")
        || lower.contains("pytest")
        || lower.contains("jest")
        || lower.contains("vitest")
        || lower.contains("node:test")
}

pub(crate) fn language_verification_artifact_role_stem(stem: &str) -> bool {
    let normalized = stem.to_ascii_lowercase().replace(['-', '.'], "_");
    matches!(
        normalized.as_str(),
        "bun_test"
            | "cargo_check"
            | "cargo_test"
            | "deno_test"
            | "dotnet_test"
            | "go_test"
            | "gradle_test"
            | "jest"
            | "mvn_test"
            | "node_test"
            | "npm_test"
            | "pnpm_test"
            | "py_compile"
            | "pytest"
            | "unittest"
            | "vitest"
            | "yarn_test"
    ) || normalized
        .split('_')
        .any(|part| matches!(part, "jest" | "vitest" | "pytest" | "unittest"))
}

pub(crate) fn language_file_refs_from_summary(summary: &str, role: ArtifactRole) -> Vec<String> {
    let mut refs = language_failure_logical_lines(summary)
        .into_iter()
        .filter_map(quoted_file_frame_path)
        .filter(|path| !language_runtime_traceback_frame_path(path))
        .filter(|path| classify_artifact_target(path).role == role)
        .map(|path| {
            path.replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or(path.as_str())
                .to_string()
        })
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    refs
}

pub(crate) fn language_failure_paths_from_summary(summary: &str) -> Vec<String> {
    let mut paths = language_failure_logical_lines(summary)
        .into_iter()
        .filter_map(quoted_file_frame_path)
        .filter(|path| !language_runtime_traceback_frame_path(path))
        .collect::<Vec<_>>();
    paths.extend(language_import_error_module_paths_from_summary(summary));
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn language_source_targets_from_text(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for token in language_path_candidate_tokens(text) {
        let Some(candidate) = normalize_language_path_candidate(&token) else {
            continue;
        };
        let spec = classify_artifact_target(&candidate);
        if spec.role == ArtifactRole::Source
            && !targets
                .iter()
                .any(|existing| existing == &spec.normalized_target)
        {
            targets.push(spec.normalized_target);
        }
    }
    targets
}

pub(crate) fn language_source_targets_from_text_handles_line_column_call_site_fixture_passes()
-> bool {
    language_source_targets_from_text("at renderOperation (src/workflow.ts:42:7)")
        == vec!["src/workflow.ts".to_string()]
}

fn language_path_candidate_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '\\' | '.' | '_' | '-' | ':') {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn normalize_language_path_candidate(token: &str) -> Option<String> {
    let mut value = token
        .trim()
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        })
        .replace('\\', "/");
    if value.is_empty() || !value.contains('.') {
        return None;
    }
    while let Some((base, suffix)) = value.rsplit_once(':') {
        if base.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            break;
        }
        value = base.to_string();
    }
    (!value.is_empty()).then_some(value)
}

pub(crate) fn language_failure_requirement_contexts_from_sources(
    labels: &[String],
    test_sources: &[String],
    limit: usize,
) -> Vec<String> {
    if labels.is_empty() || test_sources.is_empty() {
        return Vec::new();
    }

    let mut contexts = Vec::new();
    for label in labels {
        let mut ids = Vec::new();
        for source in test_sources {
            ids.extend(language_requirement_ids_for_failure_label(label, source));
        }
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            contexts.push(format!("{} -> {}", label, ids.join(", ")));
        }
        if contexts.len() >= limit {
            break;
        }
    }
    contexts.sort();
    contexts.dedup();
    contexts
}

pub(crate) fn language_failure_assertion_contexts_from_sources(
    compact_summary: &str,
    labels: &[String],
    test_sources: &[String],
    limit: usize,
) -> Vec<String> {
    if labels.is_empty() || test_sources.is_empty() {
        return Vec::new();
    }
    let subjects = language_local_boolean_assertion_subjects(compact_summary);
    if subjects.is_empty() {
        return Vec::new();
    }

    let mut contexts = Vec::new();
    for label in labels {
        for source in test_sources {
            let Some(context) =
                language_local_boolean_assertion_context_for_label(label, source, &subjects)
            else {
                continue;
            };
            contexts.push(format!("{label}: {}", context.join(" | ")));
            break;
        }
        if contexts.len() >= limit {
            break;
        }
    }
    contexts.sort();
    contexts.dedup();
    contexts
}

pub(crate) fn language_generated_test_local_binding_contradictions(
    labels: &[String],
    target_sources: &[(String, String)],
    raw_summary: &str,
) -> Vec<LanguageLocalBindingContradiction> {
    if labels.is_empty() || target_sources.is_empty() {
        return Vec::new();
    }
    let assertion_subjects = language_local_assertion_subjects(raw_summary);
    if assertion_subjects.is_empty() {
        return Vec::new();
    }

    let mut contradictions = Vec::new();
    for (target, source) in target_sources {
        for label in labels {
            if let Some(contradiction) = language_local_binding_contradiction_for_label(
                target,
                label,
                source,
                &assertion_subjects,
            ) {
                contradictions.push(contradiction);
            }
        }
    }
    contradictions
}

pub(crate) fn language_source_parse_defect(summary: &str) -> Option<LanguageParseDefect> {
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some(detail) = source_parse_defect_detail_from_line(line) else {
            continue;
        };
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..=index]);
        return Some(LanguageParseDefect {
            detail,
            path,
            line: line_number,
        });
    }
    None
}

pub(crate) fn language_source_import_time_name_resolution_defect(
    summary: &str,
) -> Option<LanguageNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("importerror: failed to import test module")
        || !lower.contains("nameerror:")
        || !lower.contains(" is not defined")
    {
        return None;
    }
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some((missing_name, suggested_name)) = source_import_time_name_error_detail(line)
        else {
            continue;
        };
        let (path, line_number) =
            source_import_time_name_resolution_location_before(&logical_lines[..index]);
        if path.is_none() {
            continue;
        }
        return Some(LanguageNameResolutionDefect {
            missing_name,
            suggested_name,
            path,
            line: line_number,
        });
    }
    None
}

pub(crate) fn language_generated_test_name_resolution_defect(
    summary: &str,
) -> Option<LanguageNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("nameerror:") || !lower.contains(" is not defined") {
        return None;
    }
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some((missing_name, suggested_name)) = source_import_time_name_error_detail(line)
        else {
            continue;
        };
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..index]);
        if classify_path_role(path.as_deref()) != Some(ArtifactRole::Test) {
            continue;
        }
        return Some(LanguageNameResolutionDefect {
            missing_name,
            suggested_name,
            path,
            line: line_number,
        });
    }
    None
}

pub(crate) fn language_generated_test_reflection_api_misuse(
    summary: &str,
) -> Option<LanguageNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || !lower.contains("code object was expected, got str")
        || !lower.contains("inspect.getsource(")
        || !lower.contains("__module__")
    {
        return None;
    }
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.contains("inspect.getsource(") || !trimmed.contains("__module__") {
            continue;
        }
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..index]);
        if classify_path_role(path.as_deref()) != Some(ArtifactRole::Test) {
            continue;
        }
        return Some(LanguageNameResolutionDefect {
            missing_name: "inspect.getsource(__module__ string)".to_string(),
            suggested_name: None,
            path,
            line: line_number,
        });
    }
    None
}

pub(crate) fn language_generated_test_module_attribute_api_misuse(
    summary: &str,
) -> Option<LanguageNameResolutionDefect> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("attributeerror:")
        || !lower.contains(" has no attribute ")
        || !lower.contains("file \"")
    {
        return None;
    }
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let Some((receiver, member)) = module_attribute_error_detail(line) else {
            continue;
        };
        if !generated_test_non_source_module_receiver(&receiver) {
            continue;
        }
        let (path, line_number) = source_parse_defect_location_before(&logical_lines[..index]);
        if classify_path_role(path.as_deref()) != Some(ArtifactRole::Test) {
            continue;
        }
        return Some(LanguageNameResolutionDefect {
            missing_name: format!("{receiver}.{member}"),
            suggested_name: None,
            path,
            line: line_number,
        });
    }
    None
}

pub(crate) fn language_generated_test_subprocess_output_capture_missing(
    summary: &str,
) -> Option<LanguageOutputStreamMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("typeerror:")
        && lower.contains("nonetype")
        && lower.contains("not iterable"))
    {
        return None;
    }
    if language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty() {
        return None;
    }
    generated_test_output_assertion(summary).map(|mut mismatch| {
        mismatch.observed_value = format!("CompletedProcess.{} is None", mismatch.stream);
        mismatch.observed_output = format!(
            "CompletedProcess.{} is None because generated subprocess.run did not capture {}",
            mismatch.stream, mismatch.stream
        );
        mismatch
    })
}

pub(crate) fn language_generated_test_subprocess_encoding_missing(
    summary: &str,
) -> Option<LanguageOutputStreamMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("unicodedecodeerror")
        && lower.contains("utf-8")
        && lower.contains("subprocess.py")
        && lower.contains("_readerthread")
        && lower.contains("nonetype")
        && lower.contains("not iterable"))
    {
        return None;
    }
    if language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty() {
        return None;
    }
    generated_test_output_assertion(summary).map(|mut mismatch| {
        mismatch.observed_value = format!(
            "CompletedProcess.{} is None after UnicodeDecodeError",
            mismatch.stream
        );
        mismatch.observed_output = format!(
            "UnicodeDecodeError while parent decoded child subprocess {} as UTF-8 without explicit child UTF-8 output authority",
            mismatch.stream
        );
        mismatch
    })
}

pub(crate) fn language_generated_test_logging_contract_overreach(
    summary: &str,
) -> Option<LanguageGeneratedTestLoggingContractOverreach> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("assertlogs(") || !lower.contains("no logs of level") {
        return None;
    }
    if language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty() {
        return None;
    }
    let assertion_line = language_failure_logical_lines(summary)
        .into_iter()
        .find(|line| line.to_ascii_lowercase().contains("assertlogs("))?
        .to_string();
    Some(LanguageGeneratedTestLoggingContractOverreach {
        logger_name: extract_assert_logs_logger(&assertion_line),
        level: extract_assert_logs_level(&assertion_line),
        assertion_line,
    })
}

pub(crate) fn language_public_output_stream_assertion_mismatch(
    summary: &str,
) -> Option<LanguageOutputStreamMismatch> {
    let logical_lines = language_failure_logical_lines(summary);
    for (index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(assert_start) = trimmed.find("self.assertIn(") {
            let Some(stream) = public_output_stream_subject(trimmed) else {
                continue;
            };
            let after = &trimmed[assert_start + "self.assertIn(".len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let args = top_level_arguments(after[..end].trim());
            let Some(expected) = args
                .first()
                .map(|value| clean_output_assertion_value(value))
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let observed = subsequent_assertion_not_found_observed_value(&logical_lines, index)
                .unwrap_or_else(|| format!("unmatched {stream} output"));
            let observed_output = if observed.is_empty() {
                format!("empty {stream}")
            } else {
                format!("{stream} `{observed}`")
            };
            return Some(LanguageOutputStreamMismatch {
                stream: stream.to_string(),
                expected_substring: expected,
                observed_value: observed,
                observed_output,
                assertion_line: trimmed.to_string(),
            });
        }
        if let Some(assert_start) = trimmed.find("self.assertEqual(") {
            let after = &trimmed[assert_start + "self.assertEqual(".len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let args = top_level_arguments(after[..end].trim());
            if args.len() < 2 {
                continue;
            }
            let Some((stream, expected)) = public_output_assert_equal_stream_and_expected(&args)
            else {
                continue;
            };
            let (observed, error_expected) =
                subsequent_assertion_equal_observed_expected_values(&logical_lines, index)
                    .unwrap_or_else(|| (format!("unmatched {stream} output"), expected.clone()));
            let expected = if !error_expected.is_empty() {
                error_expected
            } else {
                expected
            };
            let observed_output = if observed.is_empty() {
                format!("empty {stream}")
            } else {
                format!("{stream} `{observed}`")
            };
            return Some(LanguageOutputStreamMismatch {
                stream: stream.to_string(),
                expected_substring: expected,
                observed_value: observed,
                observed_output,
                assertion_line: trimmed.to_string(),
            });
        }
    }
    None
}

pub(crate) fn language_generated_test_public_output_contract_overreach(
    summary: &str,
) -> Option<LanguageOutputStreamMismatch> {
    let mismatch = language_public_output_stream_assertion_mismatch(summary)?;
    if mismatch.stream != "stdout"
        || language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty()
    {
        return None;
    }
    (public_output_values_are_same_scalar_with_decorative_formatting(
        &mismatch.expected_substring,
        &mismatch.observed_value,
    ) || public_output_assertion_is_ungrounded_process_lifecycle(summary, &mismatch))
    .then_some(mismatch)
}

pub(crate) fn language_generated_test_contract_drift_markers_from_summary(
    summary: &str,
) -> Vec<String> {
    let lower = summary.to_ascii_lowercase();
    if !(lower.contains("traceback")
        && lower.contains("self.assertequal(")
        && lower.contains("raise ")
        && lower.contains("error: test_"))
    {
        return Vec::new();
    }
    let logical_lines = language_failure_logical_lines(summary);
    let has_test_frame = logical_lines
        .iter()
        .any(|line| language_generated_test_traceback_frame_line(line));
    let has_source_raise_frame = logical_lines.windows(2).any(|window| {
        let [frame, code] = window else {
            return false;
        };
        language_source_module_traceback_frame_line(frame)
            && code.trim_start().starts_with("raise ")
    });
    if has_test_frame && has_source_raise_frame {
        vec![
            "generated-test contract contradiction: test expects a returned value while source raises a public exception".to_string(),
            "generated-test conflict evidence".to_string(),
        ]
    } else {
        Vec::new()
    }
}

pub(crate) fn language_public_state_assertions(summary: &str) -> Vec<String> {
    let mut assertions = public_state_assertions_from_normalized_feedback(summary);
    assertions.extend(public_collection_access_failures(summary));
    let logical_lines = language_failure_logical_lines(summary);
    for (line_index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        for marker in [
            "self.assertTrue(",
            "self.assertFalse(",
            "self.assertEqual(",
            "self.assertNotEqual(",
            "self.assertAlmostEqual(",
            "self.assertLess(",
            "self.assertLessEqual(",
            "self.assertGreater(",
            "self.assertGreaterEqual(",
        ] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let inside = after[..end].trim();
            let subject = first_call_argument(inside).unwrap_or(inside).trim();
            if subject.is_empty() {
                continue;
            }
            let subject = enriched_assertion_subject(&logical_lines[..line_index], subject);
            if !assertions
                .iter()
                .any(|existing: &String| existing == &subject)
            {
                assertions.push(subject);
            }
        }
    }
    assertions
}

pub(crate) fn language_public_state_assertion_observations(summary: &str) -> Vec<String> {
    let mut observations = public_state_observations_from_normalized_feedback(summary);
    observations.extend(public_collection_access_observations(summary));
    let logical_lines = language_failure_logical_lines(summary);
    for (line_index, line) in logical_lines.iter().enumerate() {
        let trimmed = line.trim();
        for marker in [
            "self.assertTrue(",
            "self.assertFalse(",
            "self.assertEqual(",
            "self.assertNotEqual(",
            "self.assertAlmostEqual(",
            "self.assertLess(",
            "self.assertLessEqual(",
            "self.assertGreater(",
            "self.assertGreaterEqual(",
        ] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let Some(end) = after.rfind(')') else {
                continue;
            };
            let inside = after[..end].trim();
            let args = top_level_arguments(inside);
            let Some(subject) = args
                .first()
                .map(|arg| arg.trim())
                .filter(|arg| !arg.is_empty())
            else {
                continue;
            };
            let subject = enriched_assertion_subject(&logical_lines[..line_index], subject);
            let expected = expected_value_for_assertion(marker, &args);
            let actual = assertion_error_actual_value(logical_lines.get(line_index + 1).copied());
            let observation = match (expected, actual) {
                (Some(expected), Some(actual)) => {
                    format!("`{subject}` expected `{expected}` but observed `{actual}`")
                }
                (Some(expected), None) => format!("`{subject}` expected `{expected}`"),
                (None, Some(actual)) => format!("`{subject}` observed `{actual}`"),
                (None, None) => format!("`{subject}`"),
            };
            if !observations
                .iter()
                .any(|existing: &String| existing == &observation)
            {
                observations.push(observation);
            }
        }
    }
    observations
}

pub(crate) fn language_public_state_terminal_transition_obligations(summary: &str) -> Vec<String> {
    let logical_lines = language_failure_logical_lines(summary);
    let mut obligations = Vec::new();
    for line in logical_lines {
        let trimmed = line.trim();
        let Some(start) = trimmed.find("self.assertEqual(") else {
            continue;
        };
        let after = &trimmed[start + "self.assertEqual(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(after[..end].trim());
        let Some(subject) = args.first().map(|arg| arg.trim()) else {
            continue;
        };
        let Some(expected) = args.get(1).map(|arg| arg.trim()) else {
            continue;
        };
        if !is_public_state_subject(subject) || !is_terminal_state_expected(expected) {
            continue;
        }
        let obligation = format!("{subject} terminal transition to {expected}");
        if !obligations
            .iter()
            .any(|existing: &String| existing == &obligation)
        {
            obligations.push(obligation);
        }
    }
    obligations
}

pub(crate) fn language_public_missing_attributes(summary: &str) -> Vec<String> {
    let mut attributes = public_missing_attributes_from_normalized_feedback(summary);
    attributes.extend(language_public_writable_property_obligations(summary));
    for line in language_failure_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        if !detail.contains(" has no attribute ") {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let attr = format!("{}.{}", quoted[0].trim(), quoted[1].trim());
        if !attributes.iter().any(|existing| existing == &attr) {
            attributes.push(attr);
        }
    }
    attributes
}

pub(crate) fn language_public_writable_property_obligations(summary: &str) -> Vec<String> {
    let mut obligations = Vec::new();
    for line in language_failure_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        let detail = detail.trim();
        if !detail.contains("property ")
            || !detail.contains(" object has no setter")
            || !detail.contains(" of ")
        {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let property = quoted[0].trim();
        let owner = quoted[1].trim();
        if property.is_empty() || owner.is_empty() {
            continue;
        }
        let obligation = format!("{owner}.{property} writable property");
        if !obligations
            .iter()
            .any(|existing: &String| existing == &obligation)
        {
            obligations.push(obligation);
        }
    }
    obligations
}

pub(crate) fn language_public_missing_method_attributes(
    summary: &str,
) -> Vec<LanguagePublicMissingMethodAttribute> {
    let logical_lines = language_failure_logical_lines(summary);
    let mut methods = Vec::new();
    for (line_index, line) in logical_lines.iter().enumerate() {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        if !detail.contains(" has no attribute ") {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let receiver = quoted[0].trim();
        let member = quoted[1].trim();
        let Some(call_site) = missing_method_call_site_before(&logical_lines[..line_index], member)
        else {
            continue;
        };
        let attribute = format!("{receiver}.{member}");
        if !methods
            .iter()
            .any(|existing: &LanguagePublicMissingMethodAttribute| existing.attribute == attribute)
        {
            methods.push(LanguagePublicMissingMethodAttribute {
                attribute,
                call_site,
            });
        }
    }
    methods
}

pub(crate) fn language_public_class_or_enum_missing_members(summary: &str) -> Vec<String> {
    let mut members = Vec::new();
    for detail in language_public_class_or_enum_missing_member_details(summary) {
        let member = detail.member;
        if !members.iter().any(|existing| existing == &member) {
            members.push(member);
        }
    }
    members
}

pub(crate) fn language_public_class_or_enum_missing_member_details(
    summary: &str,
) -> Vec<LanguagePublicClassOrEnumMissingMemberDetail> {
    let mut details = Vec::new();
    for line in language_failure_logical_lines(summary) {
        let Some(detail) = line.split("AttributeError:").nth(1) else {
            continue;
        };
        let detail = detail.trim();
        if !(detail.starts_with("type object ") || detail.starts_with("module "))
            || !detail.contains(" has no attribute ")
        {
            continue;
        }
        let quoted = quoted_segments(detail);
        if quoted.len() < 2 {
            continue;
        }
        let owner = quoted[0].trim();
        let missing = quoted[1].trim();
        let member = format!("{owner}.{missing}");
        if details
            .iter()
            .any(|existing: &LanguagePublicClassOrEnumMissingMemberDetail| {
                existing.member == member
            })
        {
            continue;
        }
        let suggested_existing_member =
            extract_quoted_after(detail, "Did you mean: '").map(|suggested| {
                if suggested.contains('.') {
                    suggested
                } else {
                    format!("{owner}.{suggested}")
                }
            });
        let expected_value = expected_value_for_class_member(summary, &member);
        details.push(LanguagePublicClassOrEnumMissingMemberDetail {
            member,
            suggested_existing_member,
            expected_value,
        });
    }
    details
}

pub(crate) fn language_public_class_member_repair_observations(summary: &str) -> Vec<String> {
    language_public_class_or_enum_missing_member_details(summary)
        .into_iter()
        .map(|detail| {
            let mut observation = format!("`{}` is missing", detail.member);
            if let Some(suggested) = detail.suggested_existing_member {
                observation.push_str(&format!("; source near-name candidate is `{suggested}`"));
            }
            if let Some(expected) = detail.expected_value {
                observation.push_str(&format!(
                    "; generated-test value contract expects `{}.value == {expected}`",
                    detail.member
                ));
            }
            observation
        })
        .collect()
}

pub(crate) fn language_public_constructor_signature_mismatch(
    summary: &str,
) -> Option<LanguagePublicConstructorSignatureMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || !lower.contains("__init__()")
        || !(lower.contains("unexpected keyword argument")
            || lower.contains("positional argument")
            || lower.contains("takes "))
    {
        return None;
    }

    let logical_lines = language_failure_logical_lines(summary);
    let detail_index = logical_lines.iter().position(|line| {
        let lower_line = line.to_ascii_lowercase();
        lower_line.contains("typeerror:") && lower_line.contains("__init__()")
    })?;
    let detail = logical_lines[detail_index]
        .split("TypeError:")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let constructor = detail
        .split(".__init__()")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let unexpected_keyword = extract_quoted_after(&detail, "unexpected keyword argument '");
    let call_site =
        constructor_call_site_before(&logical_lines[..detail_index], constructor.as_str());

    Some(LanguagePublicConstructorSignatureMismatch {
        constructor,
        detail,
        unexpected_keyword,
        call_site,
    })
}

pub(crate) fn language_public_callable_signature_mismatch(
    summary: &str,
) -> Option<LanguagePublicCallableSignatureMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("typeerror:")
        || lower.contains("__init__()")
        || !(lower.contains("missing")
            || lower.contains("required positional argument")
            || lower.contains("takes "))
    {
        return None;
    }

    let logical_lines = language_failure_logical_lines(summary);
    let detail_index = logical_lines.iter().position(|line| {
        let lower_line = line.to_ascii_lowercase();
        lower_line.contains("typeerror:")
            && !lower_line.contains("__init__()")
            && (lower_line.contains("required positional argument")
                || lower_line.contains("positional arguments")
                || lower_line.contains("takes "))
    })?;
    let detail = logical_lines[detail_index]
        .split("TypeError:")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let callable = detail
        .split("()")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    if callable
        .rsplit('.')
        .next()
        .is_some_and(|name| name == "__init__")
    {
        return None;
    }
    let missing_arguments = missing_required_arguments_from_type_error(&detail);
    let call_site = callable_call_site_before(&logical_lines[..detail_index], &callable);
    let source_target = callable_source_target_from_name(&callable);

    Some(LanguagePublicCallableSignatureMismatch {
        callable,
        detail,
        missing_arguments,
        call_site,
        source_target,
    })
}

pub(crate) fn language_public_constructor_sibling_data_shape_observations(
    summary: &str,
) -> Vec<String> {
    let Some(mismatch) = language_public_constructor_signature_mismatch(summary) else {
        return Vec::new();
    };
    public_constructor_sibling_data_shape_obligations(summary, &mismatch.constructor)
}

pub(crate) fn language_public_constructor_body_exception_observation(
    summary: &str,
) -> Option<LanguagePublicConstructorBodyExceptionObservation> {
    language_public_constructor_body_exception(summary)
}

pub(crate) fn language_public_constructor_body_exception(
    summary: &str,
) -> Option<LanguagePublicConstructorBodyExceptionObservation> {
    let logical_lines = language_failure_logical_lines(summary);
    if let Some(observation) =
        public_constructor_body_exception_from_public_exception_chain(&logical_lines, summary)
    {
        return Some(observation);
    }
    for (index, line) in logical_lines.iter().enumerate() {
        if !language_generated_test_traceback_frame_line(line) {
            continue;
        }
        let Some(call_line) = logical_lines.get(index + 1) else {
            continue;
        };
        let Some(constructor_call_site) = public_constructor_body_call_site(call_line) else {
            continue;
        };
        let Some(constructor_name) = public_constructor_name_from_call(&constructor_call_site)
        else {
            continue;
        };
        let search_tail = &logical_lines[index + 2..];
        let Some((source_initializer_call, source_failure_site, actual_exception)) =
            source_constructor_body_exception_after(search_tail)
        else {
            continue;
        };
        let sibling_constructor_obligations =
            public_constructor_signature_obligations(summary, &constructor_name);
        return Some(LanguagePublicConstructorBodyExceptionObservation {
            constructor_call_site,
            source_initializer_call,
            source_failure_site,
            actual_exception,
            sibling_constructor_obligations,
        });
    }
    public_constructor_body_exception_from_source_chain(&logical_lines, summary)
}

pub(crate) fn language_public_exception_mismatch(
    summary: &str,
) -> Option<LanguagePublicExceptionMismatch> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("traceback")
        || language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty()
    {
        return None;
    }
    if let Some(not_raised) = language_public_expected_exception_not_raised(summary) {
        return Some(LanguagePublicExceptionMismatch {
            actual_exception: format!("{} not raised", not_raised.expected_exception),
            expected_exception: Some(not_raised.expected_exception),
            call_site: not_raised.call_site,
            source_site: None,
        });
    }
    let logical_lines = language_failure_logical_lines(summary);
    let actual_index = logical_lines
        .iter()
        .rposition(|line| exception_name_from_line(line).is_some())?;
    let actual_exception = exception_name_from_line(logical_lines[actual_index])?;
    let expected_exception = expected_public_exception_name_before_actual(
        &logical_lines[..actual_index],
        &actual_exception,
    );
    let call_site = public_exception_call_site_before(&logical_lines[..actual_index]);
    let source_site = public_exception_source_site_before(&logical_lines[..actual_index])?;
    Some(LanguagePublicExceptionMismatch {
        actual_exception,
        expected_exception,
        call_site,
        source_site: Some(source_site),
    })
}

pub(crate) fn language_public_api_data_model_semantic_obligations(summary: &str) -> Vec<String> {
    let mut obligations = Vec::new();

    if let Some(mismatch) = language_public_constructor_signature_mismatch(summary) {
        let keywords = mismatch
            .call_site
            .as_deref()
            .map(call_site_keyword_arguments)
            .unwrap_or_default();
        if keywords.is_empty() {
            obligations.push(format!(
                "constructor keyword compatibility for `{}`",
                mismatch.constructor
            ));
        } else {
            obligations.push(format!(
                "constructor keyword compatibility for `{}` fields ({})",
                mismatch.constructor,
                keywords
                    .iter()
                    .map(|keyword| format!("`{keyword}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if has_enum_primitive_value_assertion(summary) {
        obligations.push("enum primitive value representation".to_string());
    }

    for observation in language_public_state_assertion_observations(summary) {
        obligations.push(format!(
            "public state assertion compatibility for {observation}"
        ));
    }
    for subject in language_public_state_assertions(summary) {
        obligations.push(format!(
            "caller-visible public state assertion for `{subject}`"
        ));
    }

    obligations.sort();
    obligations.dedup();
    obligations
}

pub(crate) fn language_public_method_sibling_obligations(summary: &str) -> Vec<String> {
    let attrs = language_public_missing_attributes(summary);
    let mut obligations = attrs
        .iter()
        .filter(|attribute| {
            let receiver = attribute.split('.').next().unwrap_or_default();
            matches!(receiver, "int" | "str" | "float" | "bool" | "list" | "dict")
        })
        .map(|attribute| format!("collection element shape defect `{attribute}`"))
        .collect::<Vec<_>>();
    obligations.sort();
    obligations.dedup();
    obligations
}

fn public_missing_attributes_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) =
        summary.split_once("Public missing-attribute mismatch detected for ")
    else {
        return Vec::new();
    };
    let end = after_marker
        .find(". Align ")
        .or_else(|| after_marker.find(". Latest "))
        .or_else(|| after_marker.find(". Required "))
        .unwrap_or(after_marker.len());
    backtick_values(&after_marker[..end])
}

fn missing_method_call_site_before(lines: &[&str], member: &str) -> Option<String> {
    let needle = format!(".{member}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("attributeerror:")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || !trimmed.contains(&needle)
        {
            return None;
        }
        Some(trimmed.to_string())
    })
}

fn expected_value_for_class_member(summary: &str, member: &str) -> Option<String> {
    let value_ref = format!("{member}.value");
    for line in language_failure_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(start) = trimmed.find("self.assertEqual(") else {
            continue;
        };
        let after = &trimmed[start + "self.assertEqual(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(&after[..end]);
        if args.first().map(|arg| arg.trim()) != Some(value_ref.as_str()) {
            continue;
        }
        return args
            .get(1)
            .map(|value| clean_assertion_scalar(value))
            .filter(|value| !value.is_empty());
    }
    None
}

fn missing_required_arguments_from_type_error(detail: &str) -> Vec<String> {
    let mut args = Vec::new();
    for marker in [
        "required positional argument: '",
        "required positional arguments: '",
        "required keyword-only argument: '",
        "required keyword-only arguments: '",
    ] {
        let Some(start) = detail.find(marker).map(|index| index + marker.len()) else {
            continue;
        };
        let rest = &detail[start..];
        let end = rest.find('\'').unwrap_or(rest.len());
        for part in rest[..end].split(" and ") {
            let value = part.trim().trim_matches('\'').trim();
            if !value.is_empty() && !args.iter().any(|existing| existing == value) {
                args.push(value.to_string());
            }
        }
    }
    args
}

fn callable_call_site_before(lines: &[&str], callable: &str) -> Option<String> {
    let terminal = callable.rsplit('.').next().unwrap_or(callable);
    let method_needle = format!(".{terminal}(");
    let function_needle = format!("{terminal}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("typeerror:")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || lower.starts_with("fail:")
            || !trimmed.contains('(')
            || !trimmed.contains(')')
        {
            return None;
        }
        if trimmed.contains(&method_needle) || trimmed.contains(&function_needle) {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

fn callable_source_target_from_name(callable: &str) -> Option<String> {
    let receiver = callable.split('.').next()?.trim();
    if receiver.is_empty()
        || matches!(
            receiver,
            "self" | "cls" | "str" | "int" | "float" | "bool" | "list" | "dict" | "tuple" | "set"
        )
    {
        return None;
    }
    if !receiver.chars().any(|ch| ch.is_ascii_uppercase()) {
        return None;
    }
    let module = upper_camel_to_snake(receiver);
    (!module.is_empty()).then(|| format!("{module}.py"))
}

fn call_site_keyword_arguments(call_site: &str) -> Vec<String> {
    let Some(arguments) = call_site
        .split_once('(')
        .and_then(|(_, tail)| tail.rsplit_once(')').map(|(inside, _)| inside))
    else {
        return Vec::new();
    };
    let mut keywords = top_level_arguments(arguments)
        .into_iter()
        .filter_map(|argument| argument.split_once('=').map(|(keyword, _)| keyword.trim()))
        .filter(|keyword| {
            !keyword.is_empty()
                && keyword
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    keywords.sort();
    keywords.dedup();
    keywords
}

fn has_enum_primitive_value_assertion(summary: &str) -> bool {
    language_failure_logical_lines(summary)
        .into_iter()
        .any(|line| {
            let Some(detail) = line.trim().strip_prefix("AssertionError:") else {
                return false;
            };
            detail.contains('<')
                && detail.contains(':')
                && detail.contains('>')
                && (detail.contains(" != '")
                    || detail.contains(" != \"")
                    || detail.contains(" != 0")
                    || detail.contains(" != 1"))
        })
}

fn upper_camel_to_snake(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        }
    }
    out
}

fn public_constructor_sibling_data_shape_obligations(
    summary: &str,
    constructor: &str,
) -> Vec<String> {
    let class_name = constructor.rsplit('.').next().unwrap_or(constructor);
    let mut obligations = Vec::new();
    for attribute in language_public_missing_attributes(summary) {
        let Some((receiver, _member)) = attribute.split_once('.') else {
            continue;
        };
        if receiver != class_name {
            continue;
        }
        let observation = format!("`{attribute}`");
        if !obligations.iter().any(|existing| existing == &observation) {
            obligations.push(observation);
        }
    }
    obligations
}

fn public_constructor_body_exception_from_public_exception_chain(
    logical_lines: &[&str],
    summary: &str,
) -> Option<LanguagePublicConstructorBodyExceptionObservation> {
    if !summary.to_ascii_lowercase().contains(" in __init__") {
        return None;
    }
    let init_index = logical_lines
        .iter()
        .position(|line| line.to_ascii_lowercase().contains(" in __init__"))?;
    let constructor_call_site = public_test_constructor_call_site(logical_lines)
        .or_else(|| {
            language_public_exception_mismatch(summary)
                .and_then(|mismatch| mismatch.call_site)
                .as_deref()
                .and_then(public_constructor_body_call_site)
        })
        .or_else(|| {
            logical_lines[..init_index]
                .iter()
                .find_map(|line| public_constructor_body_call_site(line))
        })
        .unwrap_or_else(|| "public constructor call".to_string());
    let constructor_name = public_constructor_name_from_call(&constructor_call_site)
        .unwrap_or_else(|| constructor_call_site.clone());
    let source_initializer_call = logical_lines
        .get(init_index + 1)
        .map(|value| value.trim())
        .filter(|value| public_constructor_body_code_line(value))
        .map(str::to_string);
    let source_failure_site = logical_lines
        .iter()
        .enumerate()
        .skip(init_index + 1)
        .find_map(|(index, line)| {
            if !language_source_module_traceback_frame_line(line)
                || line.to_ascii_lowercase().contains(" in __init__")
            {
                return None;
            }
            logical_lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
        });
    let actual_exception = logical_lines
        .iter()
        .skip(init_index)
        .find(|line| exception_name_from_line(line).is_some())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "constructor body exception".to_string());
    Some(LanguagePublicConstructorBodyExceptionObservation {
        constructor_call_site,
        source_initializer_call,
        source_failure_site,
        actual_exception,
        sibling_constructor_obligations: public_constructor_signature_obligations(
            summary,
            &constructor_name,
        ),
    })
}

fn public_test_constructor_call_site(logical_lines: &[&str]) -> Option<String> {
    logical_lines
        .windows(2)
        .find_map(|window| {
            if !language_generated_test_traceback_frame_line(window[0]) {
                return None;
            }
            public_constructor_body_call_site(window[1])
        })
        .or_else(|| {
            for (index, line) in logical_lines.iter().enumerate() {
                if !language_generated_test_traceback_frame_line(line) {
                    continue;
                }
                for candidate in logical_lines.iter().skip(index + 1).take(4) {
                    let lower = candidate.to_ascii_lowercase();
                    if lower.trim_start().starts_with("file ") {
                        break;
                    }
                    if let Some(call_site) = public_constructor_body_call_site(candidate) {
                        return Some(call_site);
                    }
                }
            }
            None
        })
        .or_else(|| {
            logical_lines.iter().find_map(|line| {
                let call = public_constructor_body_call_site(line)?;
                let rhs = call
                    .split_once('=')
                    .map(|(_, rhs)| rhs.trim())
                    .unwrap_or(call.as_str());
                rhs.contains('.').then_some(call)
            })
        })
}

fn public_constructor_body_exception_from_source_chain(
    logical_lines: &[&str],
    summary: &str,
) -> Option<LanguagePublicConstructorBodyExceptionObservation> {
    for (index, line) in logical_lines.iter().enumerate() {
        if !language_source_initializer_traceback_frame_line(line) {
            continue;
        }
        let Some(constructor_call_site) =
            public_exception_call_site_before(&logical_lines[..index])
                .and_then(|line| public_constructor_body_call_site(&line))
                .or_else(|| {
                    logical_lines[..index]
                        .iter()
                        .rev()
                        .find_map(|line| public_constructor_body_call_site(line))
                })
        else {
            continue;
        };
        let Some(constructor_name) = public_constructor_name_from_call(&constructor_call_site)
        else {
            continue;
        };
        let Some((source_initializer_call, source_failure_site, actual_exception)) =
            source_constructor_body_exception_after_relaxed(&logical_lines[index..])
        else {
            continue;
        };
        let sibling_constructor_obligations =
            public_constructor_signature_obligations(summary, &constructor_name);
        return Some(LanguagePublicConstructorBodyExceptionObservation {
            constructor_call_site,
            source_initializer_call,
            source_failure_site,
            actual_exception,
            sibling_constructor_obligations,
        });
    }
    public_constructor_body_exception_from_exception_projection(logical_lines, summary)
}

fn public_constructor_body_exception_from_exception_projection(
    logical_lines: &[&str],
    summary: &str,
) -> Option<LanguagePublicConstructorBodyExceptionObservation> {
    if !summary.to_ascii_lowercase().contains(" in __init__") {
        return None;
    }
    let mismatch = language_public_exception_mismatch(summary)?;
    let constructor_call_site = mismatch
        .call_site
        .as_deref()
        .and_then(public_constructor_body_call_site)?;
    let constructor_name = public_constructor_name_from_call(&constructor_call_site)?;
    let source_initializer_call = logical_lines
        .iter()
        .enumerate()
        .find(|(_, line)| language_source_initializer_traceback_frame_line(line))
        .and_then(|(index, _)| logical_lines.get(index + 1))
        .map(|value| value.trim())
        .filter(|value| public_constructor_body_code_line(value))
        .map(str::to_string);
    let source_failure_site = mismatch.source_site.as_deref().and_then(|source_site| {
        logical_lines.iter().enumerate().find_map(|(index, line)| {
            if !line.contains(source_site) || line.to_ascii_lowercase().contains(" in __init__") {
                return None;
            }
            logical_lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
        })
    });
    let actual_exception = logical_lines
        .iter()
        .find(|line| exception_name_from_line(line).as_deref() == Some(&mismatch.actual_exception))
        .map(|line| line.trim().to_string())
        .unwrap_or(mismatch.actual_exception);
    let sibling_constructor_obligations =
        public_constructor_signature_obligations(summary, &constructor_name);
    Some(LanguagePublicConstructorBodyExceptionObservation {
        constructor_call_site,
        source_initializer_call,
        source_failure_site,
        actual_exception,
        sibling_constructor_obligations,
    })
}

fn source_constructor_body_exception_after_relaxed(
    lines: &[&str],
) -> Option<(Option<String>, Option<String>, String)> {
    let mut saw_init_frame = false;
    let mut initializer_call = None;
    let mut source_failure_site = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if saw_init_frame && language_generated_test_traceback_frame_line(trimmed) {
            return None;
        }
        if language_source_initializer_traceback_frame_line(trimmed) {
            saw_init_frame = true;
            initializer_call = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string);
            continue;
        }
        if saw_init_frame && language_source_module_traceback_frame_line(trimmed) {
            source_failure_site = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
                .or(source_failure_site);
            continue;
        }
        if saw_init_frame && exception_name_from_line(trimmed).is_some() {
            return Some((initializer_call, source_failure_site, trimmed.to_string()));
        }
    }
    None
}

fn source_constructor_body_exception_after(
    lines: &[&str],
) -> Option<(Option<String>, Option<String>, String)> {
    let mut saw_init_frame = false;
    let mut initializer_call = None;
    let mut source_failure_site = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if saw_init_frame && language_generated_test_traceback_frame_line(line) {
            return None;
        }
        if language_source_initializer_traceback_frame_line(line) {
            saw_init_frame = true;
            initializer_call = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string);
            index += 1;
            continue;
        }
        if saw_init_frame && language_source_module_traceback_frame_line(line) {
            source_failure_site = lines
                .get(index + 1)
                .map(|value| value.trim())
                .filter(|value| public_constructor_body_code_line(value))
                .map(str::to_string)
                .or(source_failure_site);
            index += 1;
            continue;
        }
        if saw_init_frame && exception_name_from_line(line).is_some() {
            return Some((initializer_call, source_failure_site, line.to_string()));
        }
        index += 1;
    }
    None
}

fn public_constructor_body_call_site(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !public_constructor_body_code_line(trimmed) {
        return None;
    }
    let call = if let Some((_, rhs)) = trimmed.split_once('=') {
        rhs.trim()
    } else {
        trimmed
    };
    let name = public_constructor_name_from_call(call)?;
    if name
        .rsplit('.')
        .next()
        .and_then(|value| value.chars().next())
        .is_some_and(|ch| ch.is_ascii_uppercase())
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn public_constructor_name_from_call(call: &str) -> Option<String> {
    let call = if let Some((_, rhs)) = call.trim().split_once('=') {
        rhs.trim()
    } else {
        call.trim()
    };
    let before_paren = call.split('(').next()?.trim();
    if before_paren.is_empty()
        || before_paren.starts_with("self.")
        || before_paren.starts_with("assert")
    {
        return None;
    }
    Some(before_paren.to_string())
}

fn public_constructor_body_code_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    !line.starts_with("File ")
        && !lower.starts_with("traceback")
        && !lower.starts_with("during handling")
        && !lower.starts_with("error:")
        && !lower.starts_with("failed")
        && !lower.starts_with("raise ")
        && exception_name_from_line(line).is_none()
        && line.contains('(')
        && line.contains(')')
}

fn public_constructor_signature_obligations(summary: &str, main_constructor: &str) -> Vec<String> {
    let mut obligations = Vec::new();
    for line in language_failure_logical_lines(summary) {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("typeerror:") || !lower.contains(".__init__()") {
            continue;
        }
        let Some(detail) = line.split("TypeError:").nth(1).map(str::trim) else {
            continue;
        };
        let Some(constructor) = detail
            .split(".__init__()")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if constructor == main_constructor {
            continue;
        }
        let observation = format!("`{constructor}.__init__()`: `{detail}`");
        if !obligations.iter().any(|existing| existing == &observation) {
            obligations.push(observation);
        }
    }
    obligations
}

fn constructor_call_site_before(lines: &[&str], constructor: &str) -> Option<String> {
    let class_name = constructor.rsplit('.').next().unwrap_or(constructor);
    let needle = format!("{class_name}(");
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("typeerror:")
            || lower.starts_with("failed")
            || lower.starts_with("error:")
            || lower.starts_with("fail:")
            || !trimmed.contains(&needle)
            || !trimmed.contains(')')
        {
            return None;
        }
        Some(trimmed.to_string())
    })
}

fn expected_public_exception_name_before_actual(
    lines: &[&str],
    actual_exception: &str,
) -> Option<String> {
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        if exception_name_from_line(trimmed).is_some() {
            return None;
        }
        known_exception_name_in_text(trimmed).filter(|expected| expected != actual_exception)
    })
}

fn exception_name_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    for exception in known_public_exception_names() {
        if trimmed.starts_with(exception) && trimmed[exception.len()..].starts_with(':') {
            return Some(exception.to_string());
        }
    }
    None
}

fn known_public_exception_names() -> [&'static str; 5] {
    [
        "ZeroDivisionError",
        "ValueError",
        "TypeError",
        "RuntimeError",
        "OverflowError",
    ]
}

fn known_exception_name_in_text(text: &str) -> Option<String> {
    known_public_exception_names()
        .into_iter()
        .find(|exception| text.contains(exception))
        .map(str::to_string)
}

fn public_exception_call_site_before(lines: &[&str]) -> Option<String> {
    for window in lines.windows(2) {
        let frame = window[0].trim();
        let call = window[1].trim();
        if language_generated_test_traceback_frame_line(frame)
            && public_exception_call_site_candidate(call)
        {
            return Some(call.to_string());
        }
    }

    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        if !public_exception_call_site_candidate(trimmed) {
            return None;
        }
        Some(trimmed.to_string())
    })
}

pub(crate) fn language_public_expected_exception_not_raised(
    summary: &str,
) -> Option<LanguagePublicExpectedExceptionNotRaised> {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("traceback")
        || language_file_refs_from_summary(summary, ArtifactRole::Test).is_empty()
        || !lower.contains("assertraises")
        || !lower.contains("assertionerror:")
        || !lower.contains(" not raised")
    {
        return None;
    }
    let logical_lines = language_failure_logical_lines(summary);
    let expected_exception = logical_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let payload = trimmed.strip_prefix("AssertionError:")?.trim();
        let exception = payload.strip_suffix(" not raised")?.trim();
        (!exception.is_empty()).then(|| exception.to_string())
    })?;
    let call_site = logical_lines.iter().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.contains("assertRaises(") || trimmed.contains("assertRaisesRegex(") {
            Some(trimmed.to_string())
        } else {
            None
        }
    });
    Some(LanguagePublicExpectedExceptionNotRaised {
        expected_exception,
        call_site,
    })
}

fn public_exception_source_site_before(lines: &[&str]) -> Option<String> {
    lines.windows(2).rev().find_map(|window| {
        let frame = window[0].trim();
        if !language_source_module_traceback_frame_line(frame) {
            return None;
        }
        language_traceback_frame_path(frame)
    })
}

fn public_exception_call_site_candidate(line: &str) -> bool {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    !trimmed.starts_with("File ")
        && !lower.starts_with("traceback")
        && !lower.starts_with("during handling")
        && !lower.starts_with("error:")
        && !lower.starts_with("failed")
        && !lower.starts_with("raise ")
        && !lower.starts_with("return ")
        && exception_name_from_line(trimmed).is_none()
        && trimmed.contains('(')
        && trimmed.contains(')')
}

pub(crate) fn language_generated_test_traceback_frame_line(line: &str) -> bool {
    quoted_file_frame_path(line)
        .as_deref()
        .is_some_and(|path| classify_path_role(Some(path)) == Some(ArtifactRole::Test))
}

pub(crate) fn language_source_module_traceback_frame_line(line: &str) -> bool {
    quoted_file_frame_path(line).as_deref().is_some_and(|path| {
        classify_path_role(Some(path)) == Some(ArtifactRole::Source)
            && !language_runtime_traceback_frame_path(path)
    })
}

pub(crate) fn language_source_initializer_traceback_frame_line(line: &str) -> bool {
    language_source_module_traceback_frame_line(line)
        && line.to_ascii_lowercase().contains(" in __init__")
}

pub(crate) fn language_traceback_frame_path(line: &str) -> Option<String> {
    quoted_file_frame_path(line)
}

pub(crate) fn language_runtime_traceback_frame_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/lib/unittest/")
        || normalized.contains("/lib/site-packages/")
        || normalized.contains("/lib/python")
        || normalized.contains("/python")
            && normalized.contains("/lib/")
            && !normalized.contains("/workspace/")
            && !normalized.contains("/project_sandbox/")
}

fn language_import_error_module_paths_from_summary(summary: &str) -> Vec<String> {
    let mut paths = language_failure_logical_lines(summary)
        .into_iter()
        .filter_map(language_import_error_module_path)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn language_import_error_module_path(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("importerror:")
        || !lower.contains("cannot import name")
        || !lower.contains(" from ")
    {
        return None;
    }

    let start = line.rfind('(')?;
    let end = line[start + 1..].find(')')? + start + 1;
    let candidate = line[start + 1..end].trim();
    if candidate.is_empty() {
        return None;
    }
    let normalized = candidate.replace('\\', "/").to_ascii_lowercase();
    normalized.ends_with(".py").then(|| candidate.to_string())
}

fn normalize_python_module_verification_command(text: &str) -> String {
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
    if index + 1 < tokens.len() && tokens[index].eq_ignore_ascii_case("-m") {
        let module = tokens[index + 1].to_ascii_lowercase();
        if module != "unittest" && module != "py_compile" {
            return text.to_string();
        }
        let mut canonical = vec!["python".to_string(), "-m".to_string(), module];
        canonical.extend(tokens[index + 2..].iter().map(|token| token.to_string()));
        return canonical.join(" ");
    }

    text.to_string()
}

fn generated_test_output_assertion(summary: &str) -> Option<LanguageOutputStreamMismatch> {
    for line in language_failure_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(assert_start) = trimmed.find("self.assertIn(") else {
            continue;
        };
        let Some(stream) = public_output_stream_subject(trimmed) else {
            continue;
        };
        let after = &trimmed[assert_start + "self.assertIn(".len()..];
        let Some(end) = after.rfind(')') else {
            continue;
        };
        let args = top_level_arguments(after[..end].trim());
        let Some(expected) = args
            .first()
            .map(|value| clean_output_assertion_value(value))
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return Some(LanguageOutputStreamMismatch {
            stream: stream.to_string(),
            expected_substring: expected,
            observed_value: String::new(),
            observed_output: String::new(),
            assertion_line: trimmed.to_string(),
        });
    }
    None
}

fn public_output_stream_subject(assertion_line: &str) -> Option<&'static str> {
    if assertion_line.contains("result.stderr") || assertion_line.contains(".stderr") {
        Some("stderr")
    } else if assertion_line.contains("result.stdout") || assertion_line.contains(".stdout") {
        Some("stdout")
    } else {
        None
    }
}

fn public_output_assert_equal_stream_and_expected(args: &[&str]) -> Option<(&'static str, String)> {
    let first_stream = args
        .first()
        .and_then(|arg| public_output_stream_subject(arg));
    let second_stream = args
        .get(1)
        .and_then(|arg| public_output_stream_subject(arg));
    if let Some(stream) = first_stream {
        return args
            .get(1)
            .map(|arg| clean_output_assertion_value(arg))
            .filter(|value| !value.is_empty())
            .map(|expected| (stream, expected));
    }
    if let Some(stream) = second_stream {
        return args
            .first()
            .map(|arg| clean_output_assertion_value(arg))
            .filter(|value| !value.is_empty())
            .map(|expected| (stream, expected));
    }
    None
}

fn assertion_not_found_observed_value(line: &str) -> Option<String> {
    let detail = line.trim().strip_prefix("AssertionError:")?.trim();
    let (_, observed) = detail.split_once(" not found in ")?;
    Some(clean_output_assertion_value(observed))
}

fn subsequent_assertion_not_found_observed_value(lines: &[&str], index: usize) -> Option<String> {
    lines
        .iter()
        .skip(index + 1)
        .take(6)
        .find_map(|line| assertion_not_found_observed_value(line))
}

fn assertion_equal_observed_expected_values(line: &str) -> Option<(String, String)> {
    let detail = line.trim().strip_prefix("AssertionError:")?.trim();
    let (observed, expected) = detail.split_once("!=")?;
    Some((
        clean_output_assertion_value(observed),
        clean_output_assertion_value(expected),
    ))
}

fn subsequent_assertion_equal_observed_expected_values(
    lines: &[&str],
    index: usize,
) -> Option<(String, String)> {
    lines
        .iter()
        .skip(index + 1)
        .take(6)
        .find_map(|line| assertion_equal_observed_expected_values(line))
}

fn clean_output_assertion_value(value: &str) -> String {
    let value = value.trim().trim_end_matches(',').trim();
    if value.len() >= 2 {
        let mut chars = value.chars();
        let first = chars.next();
        let last = value.chars().last();
        if matches!(
            (first, last),
            (Some('\''), Some('\'')) | (Some('"'), Some('"'))
        ) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn extract_assert_logs_logger(assertion_line: &str) -> Option<String> {
    extract_delimited_after_char(assertion_line, "assertLogs(\"", '"')
        .or_else(|| extract_delimited_after_char(assertion_line, "assertLogs('", '\''))
}

fn extract_assert_logs_level(assertion_line: &str) -> Option<String> {
    extract_delimited_after_char(assertion_line, "level=\"", '"')
        .or_else(|| extract_delimited_after_char(assertion_line, "level='", '\''))
}

fn extract_delimited_after_char(text: &str, marker: &str, terminator: char) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find(terminator)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn public_output_assertion_is_ungrounded_process_lifecycle(
    summary: &str,
    mismatch: &LanguageOutputStreamMismatch,
) -> bool {
    let lower = summary.to_ascii_lowercase();
    if !lower.contains("eof")
        && !lower.contains("end of file")
        && !lower.contains("no input")
        && !lower.contains("empty input")
    {
        return false;
    }
    let expected = clean_output_assertion_value(&mismatch.expected_substring);
    if expected.is_empty() {
        return false;
    }
    let observed = clean_output_assertion_value(&mismatch.observed_value);
    observed.contains(">>>") || observed.is_empty() || !observed.contains(&expected)
}

fn public_output_values_are_same_scalar_with_decorative_formatting(
    expected: &str,
    observed: &str,
) -> bool {
    let expected_scalar = normalize_decorated_public_output_scalar(expected);
    let observed_scalar = normalize_public_output_observed_scalar_for_expected(expected, observed)
        .unwrap_or_else(|| normalize_decorated_public_output_scalar(observed));
    if expected_scalar.is_empty() || observed_scalar.is_empty() || expected == observed {
        return false;
    }
    match (
        expected_scalar.parse::<f64>(),
        observed_scalar.parse::<f64>(),
    ) {
        (Ok(expected), Ok(observed)) => (expected - observed).abs() < f64::EPSILON,
        _ => false,
    }
}

fn normalize_decorated_public_output_scalar(value: &str) -> String {
    let mut trimmed = clean_output_assertion_value(value);
    trimmed = trimmed.trim().to_string();
    if let Some(rest) = trimmed.strip_prefix('=') {
        trimmed = rest.trim().to_string();
    }
    if let Some((_, rest)) = trimmed.rsplit_once(':') {
        let candidate = rest.trim();
        if !candidate.is_empty() {
            trimmed = candidate.to_string();
        }
    }
    trimmed
}

fn normalize_public_output_observed_scalar_for_expected(
    expected: &str,
    observed: &str,
) -> Option<String> {
    let expected = clean_output_assertion_value(expected);
    let observed = clean_output_assertion_value(observed);
    let (label, _) = expected.split_once(':')?;
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let label_start = observed.find(label)?;
    let rest = observed[label_start + label.len()..].trim_start();
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix('：'))
        .unwrap_or(rest)
        .trim_start();
    let mut scalar = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch == 'e' || ch == 'E' {
            scalar.push(ch);
        } else if !scalar.is_empty() {
            break;
        }
    }
    (!scalar.is_empty()).then_some(scalar)
}

fn first_call_argument(arguments: &str) -> Option<&str> {
    top_level_arguments(arguments).into_iter().next()
}

fn expected_value_for_assertion(marker: &str, args: &[&str]) -> Option<String> {
    if marker.contains("assertTrue") {
        return Some("truthy".to_string());
    }
    if marker.contains("assertFalse") {
        return Some("false".to_string());
    }
    if marker.contains("assertLessEqual") {
        return args
            .get(1)
            .map(|value| format!("<= {}", clean_assertion_scalar(value)));
    }
    if marker.contains("assertLess") {
        return args
            .get(1)
            .map(|value| format!("< {}", clean_assertion_scalar(value)));
    }
    if marker.contains("assertGreaterEqual") {
        return args
            .get(1)
            .map(|value| format!(">= {}", clean_assertion_scalar(value)));
    }
    if marker.contains("assertGreater") {
        return args
            .get(1)
            .map(|value| format!("> {}", clean_assertion_scalar(value)));
    }
    args.get(1)
        .map(|value| clean_assertion_scalar(value))
        .filter(|value| !value.is_empty())
}

fn assertion_error_actual_value(line: Option<&str>) -> Option<String> {
    let line = line?.trim();
    let detail = line.strip_prefix("AssertionError:")?.trim();
    if let Some((actual, _)) = detail.split_once("!=") {
        return Some(clean_assertion_scalar(actual));
    }
    if detail.contains("False is not true") {
        return Some("False".to_string());
    }
    if detail.contains("True is not false") {
        return Some("True".to_string());
    }
    for marker in [
        " not less than or equal to ",
        " not greater than or equal to ",
        " not less than ",
        " not greater than ",
    ] {
        if let Some((actual, _expected)) = detail.split_once(marker) {
            return Some(clean_assertion_scalar(actual));
        }
    }
    None
}

fn clean_assertion_scalar(value: &str) -> String {
    value
        .split(" within ")
        .next()
        .unwrap_or(value)
        .trim()
        .trim_end_matches(',')
        .trim()
        .to_string()
}

fn enriched_assertion_subject(previous_lines: &[&str], subject: &str) -> String {
    let Some(root) = root_identifier(subject) else {
        return subject.to_string();
    };
    let Some(rhs) = previous_assignment_rhs(previous_lines, root) else {
        return subject.to_string();
    };
    if subject == root {
        format!("{root} = {rhs}")
    } else {
        format!("{subject} from {root} = {rhs}")
    }
}

fn root_identifier(subject: &str) -> Option<&str> {
    let subject = subject.trim();
    let mut end = 0usize;
    for (index, ch) in subject.char_indices() {
        if index == 0 {
            if !(ch == '_' || ch.is_ascii_alphabetic()) {
                return None;
            }
            end = ch.len_utf8();
            continue;
        }
        if ch == '_' || ch.is_ascii_alphanumeric() {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| &subject[..end])
}

fn previous_assignment_rhs<'a>(previous_lines: &'a [&'a str], variable: &str) -> Option<&'a str> {
    previous_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix(variable)?.trim_start();
        let rhs = rest.strip_prefix('=')?.trim();
        (!rhs.is_empty()).then_some(rhs)
    })
}

fn public_state_assertions_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) =
        summary.split_once("Public state assertion mismatch detected for ")
    else {
        return Vec::new();
    };
    let end = after_marker
        .find(": expected public state")
        .or_else(|| after_marker.find(". Observed mismatch"))
        .unwrap_or(after_marker.len());
    backtick_values(&after_marker[..end])
}

fn public_state_observations_from_normalized_feedback(summary: &str) -> Vec<String> {
    let Some((_, after_marker)) = summary.split_once("Observed mismatch:") else {
        return Vec::new();
    };
    let end = after_marker
        .find(". For ")
        .or_else(|| after_marker.find(". Latest "))
        .or_else(|| after_marker.find(". Do not "))
        .unwrap_or(after_marker.len());
    let mut observations = Vec::new();
    for clause in after_marker[..end].split(';') {
        let values = backtick_values(clause);
        if values.len() >= 3 {
            observations.push(format!(
                "`{}` expected `{}` but observed `{}`",
                values[0], values[1], values[2]
            ));
        }
    }
    observations
}

fn public_collection_access_failures(summary: &str) -> Vec<String> {
    let logical_lines = language_failure_logical_lines(summary);
    let mut accesses = Vec::new();
    for (line_index, line) in logical_lines.iter().enumerate() {
        if !line.contains("IndexError: list index out of range") {
            continue;
        }
        let Some(access) = preceding_collection_access(&logical_lines[..line_index]) else {
            continue;
        };
        if !accesses.iter().any(|existing| existing == &access) {
            accesses.push(access);
        }
    }
    accesses
}

fn public_collection_access_observations(summary: &str) -> Vec<String> {
    public_collection_access_failures(summary)
        .into_iter()
        .map(|access| format!("`{access}` expected collection element but observed `IndexError`"))
        .collect()
}

fn preceding_collection_access(previous_lines: &[&str]) -> Option<String> {
    previous_lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.starts_with("File ")
            || lower.starts_with("traceback")
            || lower.starts_with("error:")
            || lower.starts_with("failed")
            || !trimmed.contains('[')
            || !trimmed.contains(']')
        {
            return None;
        }
        first_collection_access(trimmed)
    })
}

fn first_collection_access(line: &str) -> Option<String> {
    let open = line.find('[')?;
    let close = line[open..].find(']')? + open;
    let mut start = open;
    while start > 0 {
        let ch = line.as_bytes()[start - 1] as char;
        if ch == '_' || ch == '.' || ch.is_ascii_alphanumeric() {
            start -= 1;
        } else {
            break;
        }
    }
    if start == open {
        return None;
    }
    Some(line[start..=close].trim().to_string())
}

fn is_public_state_subject(subject: &str) -> bool {
    let normalized = subject.trim().trim_matches('`');
    normalized == "state"
        || normalized.ends_with(".state")
        || normalized.contains(".state.")
        || normalized.ends_with("_state")
        || normalized.ends_with(".status")
        || normalized.ends_with("_status")
}

fn is_terminal_state_expected(expected: &str) -> bool {
    let normalized = expected
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_uppercase();
    normalized.contains(".WIN")
        || normalized.contains(".WON")
        || normalized.ends_with("WIN")
        || normalized.ends_with("WON")
        || normalized.contains("COMPLETE")
        || normalized.contains("COMPLETED")
        || normalized.contains("FINISH")
        || normalized.contains("ENDED")
        || normalized.contains("FAIL")
        || normalized.contains("SUCCESS")
}

fn backtick_values(text: &str) -> Vec<String> {
    text.split('`')
        .enumerate()
        .filter_map(|(index, value)| {
            (index % 2 == 1 && !value.trim().is_empty()).then(|| value.trim().to_string())
        })
        .collect()
}

fn top_level_arguments(arguments: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in arguments.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(arguments[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = arguments[start..].trim();
    if !tail.is_empty() {
        args.push(tail);
    }
    args
}

fn language_local_boolean_assertion_subjects(summary: &str) -> Vec<String> {
    let mut subjects = Vec::new();
    for line in language_failure_logical_lines(summary) {
        let trimmed = line.trim();
        for marker in ["self.assertTrue(", "self.assertFalse("] {
            let Some(start) = trimmed.find(marker) else {
                continue;
            };
            let after = &trimmed[start + marker.len()..];
            let end = after
                .find(',')
                .or_else(|| after.find(')'))
                .unwrap_or(after.len());
            let subject = after[..end].trim();
            if language_local_identifier(subject)
                && !subjects.iter().any(|existing| existing == subject)
            {
                subjects.push(subject.to_string());
            }
        }
    }
    subjects
}

fn language_local_assertion_subjects(summary: &str) -> Vec<String> {
    let mut subjects = language_local_boolean_assertion_subjects(summary);
    for line in language_failure_logical_lines(summary) {
        let trimmed = line.trim();
        let Some(assert_start) = trimmed.find("self.assert") else {
            continue;
        };
        let rest = &trimmed[assert_start..];
        let Some(open_index) = rest.find('(') else {
            continue;
        };
        let after_open = &rest[open_index + 1..];
        let end = after_open
            .find(',')
            .or_else(|| after_open.find(')'))
            .unwrap_or(after_open.len());
        let subject = after_open[..end].trim();
        if language_local_identifier(subject)
            && !subjects.iter().any(|existing| existing == subject)
        {
            subjects.push(subject.to_string());
        }
    }
    subjects.sort();
    subjects.dedup();
    subjects
}

fn language_local_binding_contradiction_for_label(
    target: &str,
    label: &str,
    source: &str,
    assertion_subjects: &[String],
) -> Option<LanguageLocalBindingContradiction> {
    let lines = source.lines().collect::<Vec<_>>();
    let method_index = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    })?;
    let method_indent = language_leading_space_count(lines[method_index]);
    let method_end = lines[method_index + 1..]
        .iter()
        .position(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty()
                && language_leading_space_count(line) <= method_indent
                && (trimmed.starts_with("def ") || trimmed.starts_with("class "))
        })
        .map(|offset| method_index + 1 + offset)
        .unwrap_or(lines.len());
    let body = &lines[method_index + 1..method_end];
    for (assertion_index, assertion_line) in body.iter().enumerate() {
        let assertion_line = assertion_line.trim();
        if !assertion_line.contains("self.assert") {
            continue;
        }
        let Some(asserted_subject) = assertion_subjects
            .iter()
            .find(|subject| language_line_contains_identifier(assertion_line, subject))
        else {
            continue;
        };
        for assignment_line in body[..assertion_index].iter().rev() {
            let assignment_line = assignment_line.trim();
            let duplicates = language_duplicate_destructuring_identifiers(assignment_line);
            if duplicates.iter().any(|item| item == asserted_subject) {
                return Some(LanguageLocalBindingContradiction {
                    test_target: target.to_string(),
                    label: label.to_string(),
                    identifier: asserted_subject.clone(),
                    assignment_line: assignment_line.to_string(),
                    assertion_line: assertion_line.to_string(),
                });
            }
        }
    }
    None
}

fn language_local_boolean_assertion_context_for_label(
    label: &str,
    source: &str,
    subjects: &[String],
) -> Option<Vec<String>> {
    let lines = source.lines().collect::<Vec<_>>();
    let method_index = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    })?;
    let method_indent = language_leading_space_count(lines[method_index]);
    let method_end = lines[method_index + 1..]
        .iter()
        .position(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty()
                && language_leading_space_count(line) <= method_indent
                && (trimmed.starts_with("def ") || trimmed.starts_with("class "))
        })
        .map(|offset| method_index + 1 + offset)
        .unwrap_or(lines.len());
    let body = &lines[method_index + 1..method_end];
    for subject in subjects {
        let Some(assertion_index) = body.iter().position(|line| {
            let trimmed = line.trim();
            trimmed.contains(&format!("assertTrue({subject}"))
                || trimmed.contains(&format!("assertFalse({subject}"))
        }) else {
            continue;
        };
        let Some(assignment_index) = body[..assertion_index]
            .iter()
            .rposition(|line| line.trim_start().starts_with(&format!("{subject} =")))
        else {
            continue;
        };
        let start = assignment_index.saturating_sub(4);
        let end = (assertion_index + 1).min(body.len());
        let context = body[start..end]
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !context.is_empty() {
            return Some(context);
        }
    }
    None
}

fn language_failure_logical_lines(summary: &str) -> Vec<&str> {
    summary
        .lines()
        .flat_map(|line| line.split('|'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn quoted_file_frame_path(frame: &str) -> Option<String> {
    let trimmed = frame.trim();
    if !trimmed.starts_with("File ") {
        return None;
    }
    let start = frame.find('"')? + 1;
    let rest = &frame[start..];
    let end = rest.find('"')?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn language_leading_space_count(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn language_local_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn language_duplicate_destructuring_identifiers(line: &str) -> Vec<String> {
    if line.contains("==") || line.contains("!=") || line.contains("<=") || line.contains(">=") {
        return Vec::new();
    }
    let Some((lhs, _)) = line.split_once('=') else {
        return Vec::new();
    };
    if !lhs.contains(',') {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for raw in lhs
        .trim()
        .trim_matches(|ch| matches!(ch, '(' | ')' | '[' | ']'))
        .split(',')
    {
        let identifier = raw.trim();
        if identifier == "_" || !language_local_identifier(identifier) {
            continue;
        }
        if !seen.insert(identifier.to_string()) {
            duplicates.insert(identifier.to_string());
        }
    }
    duplicates.into_iter().collect()
}

fn language_line_contains_identifier(line: &str, identifier: &str) -> bool {
    line.split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .any(|token| token == identifier)
}

fn language_requirement_ids_for_failure_label(label: &str, source: &str) -> Vec<String> {
    let lines = source.lines().collect::<Vec<_>>();
    let Some(method_index) = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("def ")
            && trimmed
                .strip_prefix("def ")
                .is_some_and(|rest| rest.starts_with(label) && rest[label.len()..].starts_with('('))
    }) else {
        return Vec::new();
    };
    let class_index = lines[..method_index]
        .iter()
        .rposition(|line| line.trim_start().starts_with("class "))
        .unwrap_or(method_index);
    let context_start = class_index.saturating_sub(3);
    let context_end = (method_index + 10).min(lines.len());
    extract_language_contract_requirement_ids(&lines[context_start..context_end].join("\n"))
}

fn extract_language_contract_requirement_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for raw in text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-')) {
        let token = raw.trim_matches(|ch: char| matches!(ch, ':' | '[' | ']' | '`' | '"' | '\''));
        let Some((prefix, number)) = token.split_once('-') else {
            continue;
        };
        if prefix.chars().all(|ch| ch.is_ascii_uppercase())
            && !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
        {
            ids.push(format!("{prefix}-{number}"));
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn source_parse_defect_detail_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    for marker in ["SyntaxError:", "IndentationError:", "TabError:"] {
        if let Some(start) = trimmed.find(marker) {
            return Some(trimmed[start..].trim().to_string());
        }
    }
    None
}

fn source_parse_defect_location_before(lines: &[&str]) -> (Option<String>, Option<u32>) {
    lines
        .iter()
        .rev()
        .find_map(|line| source_parse_defect_location_from_line(line))
        .unwrap_or((None, None))
}

fn source_parse_defect_location_from_line(line: &str) -> Option<(Option<String>, Option<u32>)> {
    let start = line.find("File \"")? + "File \"".len();
    let rest = &line[start..];
    let path_end = rest.find('"')?;
    let path = rest[..path_end].trim();
    let after_path = &rest[path_end..];
    let line_marker = ", line ";
    let line_start = after_path
        .find(line_marker)
        .map(|index| index + line_marker.len());
    let line_number = line_start.and_then(|index| {
        let tail = &after_path[index..];
        let digits = tail
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        digits.parse::<u32>().ok()
    });
    Some(((!path.is_empty()).then(|| path.to_string()), line_number))
}

fn source_import_time_name_error_detail(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    if !trimmed.contains("NameError:") || !trimmed.contains(" is not defined") {
        return None;
    }
    let missing_name = extract_quoted_after(trimmed, "NameError: name '")?;
    let suggested_name = extract_quoted_after(trimmed, "Did you mean: '");
    Some((missing_name, suggested_name))
}

fn module_attribute_error_detail(line: &str) -> Option<(String, String)> {
    let detail = line.split("AttributeError:").nth(1)?.trim();
    if !detail.starts_with("module ") || !detail.contains(" has no attribute ") {
        return None;
    }
    let quoted = quoted_segments(detail);
    if quoted.len() < 2 {
        return None;
    }
    let receiver = quoted[0].trim();
    let member = quoted[1].trim();
    if receiver.is_empty() || member.is_empty() {
        return None;
    }
    Some((receiver.to_string(), member.to_string()))
}

fn generated_test_non_source_module_receiver(receiver: &str) -> bool {
    let receiver = receiver.trim();
    if receiver.is_empty() || receiver.contains('.') {
        return false;
    }
    matches!(
        receiver,
        "abc"
            | "argparse"
            | "asyncio"
            | "collections"
            | "contextlib"
            | "csv"
            | "datetime"
            | "decimal"
            | "enum"
            | "functools"
            | "glob"
            | "inspect"
            | "io"
            | "itertools"
            | "json"
            | "logging"
            | "math"
            | "operator"
            | "os"
            | "pathlib"
            | "random"
            | "re"
            | "shutil"
            | "statistics"
            | "string"
            | "subprocess"
            | "sys"
            | "tempfile"
            | "textwrap"
            | "time"
            | "types"
            | "typing"
            | "unittest"
    )
}

fn quoted_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in text.chars() {
        if ch == '\'' || ch == '`' {
            if in_quote {
                if !current.trim().is_empty() {
                    segments.push(current.trim().to_string());
                }
                current.clear();
                in_quote = false;
            } else {
                in_quote = true;
            }
            continue;
        }
        if in_quote {
            current.push(ch);
        }
    }
    segments
}

fn source_import_time_name_resolution_location_before(
    lines: &[&str],
) -> (Option<String>, Option<u32>) {
    lines
        .iter()
        .rev()
        .filter_map(|line| source_parse_defect_location_from_line(line))
        .find(|(path, _)| {
            path.as_deref()
                .is_some_and(source_import_time_name_resolution_source_frame)
        })
        .unwrap_or((None, None))
}

fn source_import_time_name_resolution_source_frame(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    classify_artifact_target(path).role == ArtifactRole::Source
        && normalized.ends_with(".py")
        && !normalized.contains("/python")
        && !normalized.contains("/lib/unittest/")
}

fn classify_path_role(path: Option<&str>) -> Option<ArtifactRole> {
    path.map(classify_artifact_target).map(|spec| spec.role)
}

fn extract_quoted_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find('\'')?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn code_like_test_target(lower: &str) -> bool {
    let file_name = lower.rsplit('/').next().unwrap_or(lower);
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    let code_like_ext = matches!(
        ext,
        "js" | "ts"
            | "tsx"
            | "jsx"
            | "rs"
            | "go"
            | "java"
            | "kt"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "scala"
    );
    code_like_ext
        && (stem.starts_with("test_")
            || stem.ends_with("_test")
            || stem.ends_with(".test")
            || stem.ends_with("-test")
            || stem.ends_with("_spec")
            || stem.ends_with(".spec")
            || stem.ends_with("-spec")
            || lower.contains("/tests/")
            || lower.contains("/__tests__/"))
}

fn code_like_source_target(lower: &str) -> bool {
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some(
            "rs" | "js"
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
        )
    )
}

fn python_test_target_projection(target: &str) -> Option<(String, String, String)> {
    let (dir, name) = target
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name.to_string()))
        .unwrap_or_else(|| (String::new(), target.to_string()));
    let stem = name.strip_suffix(".py")?;
    let module = stem
        .strip_prefix("test_")
        .or_else(|| stem.strip_suffix("_test"))?;
    if module.trim().is_empty() {
        return None;
    }
    Some((
        format!("{dir}{module}.py"),
        module.to_string(),
        format!("Test{}", snake_to_pascal(module)),
    ))
}

fn snake_to_pascal(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn language_evidence_adapter_registry_fixture_passes() -> bool {
    let test = classify_artifact_target("tests/test_workflow.py");
    let source = classify_artifact_target("src/workflow.py");
    let docs = classify_artifact_target("docs/workflow-design.md");
    let unknown = classify_artifact_target("assets/workflow.bin");
    let js_test = classify_artifact_target("src/workflow.spec.ts");
    let tsx_test = classify_artifact_target("src/workflow.test.tsx");
    let root_rust_test = classify_artifact_target("test_workflow.rs");

    test.language == LanguageFamily::Python
        && test.role == ArtifactRole::Test
        && test.source_path.as_deref() == Some("tests/workflow.py")
        && test.module_name.as_deref() == Some("workflow")
        && test.class_name.as_deref() == Some("TestWorkflow")
        && source.language == LanguageFamily::Python
        && source.role == ArtifactRole::Source
        && classify_artifact_target("src/lib.rs").language == LanguageFamily::Code
        && classify_artifact_target("src/lib.rs").role == ArtifactRole::Source
        && js_test.language == LanguageFamily::Code
        && js_test.role == ArtifactRole::Test
        && tsx_test.language == LanguageFamily::Code
        && tsx_test.role == ArtifactRole::Test
        && root_rust_test.language == LanguageFamily::Code
        && root_rust_test.role == ArtifactRole::Test
        && root_rust_test.source_path.as_deref() == Some("workflow.rs")
        && docs.language == LanguageFamily::Text
        && docs.role == ArtifactRole::Document
        && unknown.language == LanguageFamily::Unknown
        && unknown.role == ArtifactRole::Unknown
        && normalize_language_verification_command("python -X utf8 -m unittest tests")
            == "python -m unittest tests"
        && looks_like_language_explicit_verification_command("python -m py_compile src/workflow.py")
        && looks_like_language_explicit_verification_command("npm test")
        && looks_like_language_explicit_verification_command("cargo test --all")
        && looks_like_language_explicit_verification_command("verify-contract --schema src/workflow.rs")
        && language_verification_command_evidence("cargo build")
        && language_build_check_verification_evidence("python -m py_compile src/workflow.py")
        && !language_test_runner_evidence("cargo build")
        && !language_test_runner_evidence("python -m py_compile src/workflow.py")
        && language_command_text_io_surface_evidence(
            &["bun".to_string(), "test".to_string()],
            "bun test",
        )
        && language_command_text_io_surface_evidence(
            &["deno".to_string(), "test".to_string()],
            "deno test",
        )
        && language_command_test_or_verification_io_evidence(
            &["cargo".to_string(), "test".to_string()],
            "cargo test",
        )
        && language_runtime_execution_io_evidence(&["node".to_string(), "tool.js".to_string()])
        && language_command_inherits_utf8_bootstrap(&["python".to_string()])
        && language_python_utf8_correction_applies(&["pytest".to_string()])
        && looks_like_language_direct_shell_verification_command("python src/workflow.py")
        && looks_like_language_direct_shell_verification_command("node --test")
        && looks_like_language_direct_shell_verification_command("deno test")
        && language_verification_failure_summary_evidence(
            "Command: python -m unittest\nFAILED test_workflow",
        )
        && language_verification_failure_summary_evidence("Command: npm test\nFAIL workflow.spec.ts")
        && language_verification_artifact_role_stem("vitest")
        && language_verification_artifact_role_stem("npm-test")
        && !language_verification_artifact_role_stem("test_plan")
        && language_verification_target_candidates("test_workflow.TestWorkflow")
            .starts_with(&[
                "test_workflow/TestWorkflow.py".to_string(),
                "test_workflow.TestWorkflow.py".to_string(),
                "tests/test_workflow/TestWorkflow.py".to_string(),
                "tests/test_workflow.TestWorkflow.py".to_string(),
            ])
        && language_verification_target_candidates("workflow.spec")
            .contains(&"src/workflow.spec.tsx".to_string())
        && language_source_parse_defect(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 3\nSyntaxError: expected ':'",
        )
        .is_some_and(|defect| defect.path.as_deref() == Some("tests/test_workflow.py"))
        && language_generated_test_subprocess_output_capture_missing(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 4, in test_cli\n    self.assertIn(\"ok\", result.stdout)\nTypeError: argument of type 'NoneType' is not iterable",
        )
        .is_some_and(|mismatch| mismatch.stream == "stdout")
        && language_generated_test_logging_contract_overreach(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_logging\n    with self.assertLogs(\"workflow\", level=\"INFO\"):\nAssertionError: no logs of level INFO or higher triggered on workflow",
        )
        .is_some_and(|overreach| {
            overreach.logger_name.as_deref() == Some("workflow")
                && overreach.level.as_deref() == Some("INFO")
        })
        && language_generated_test_public_output_contract_overreach(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 5, in test_render_output\n    self.assertIn(\"value: 3\", result.stdout)\nAssertionError: 'value: 3' not found in '3'",
        )
        .is_some_and(|mismatch| mismatch.expected_substring == "value: 3")
        && language_generated_test_reflection_api_misuse(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 6, in test_reflection\n    inspect.getsource(workflow.__module__)\nTypeError: module, class, method, function, traceback, frame, or code object was expected, got str",
        )
        .is_some_and(|defect| defect.missing_name == "inspect.getsource(__module__ string)")
        && language_generated_test_module_attribute_api_misuse(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 6, in test_cli\n    os.path\nAttributeError: module 'os' has no attribute 'path'",
        )
        .is_some_and(|defect| defect.missing_name == "os.path")
        && language_generated_test_contract_drift_markers_from_summary(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 9, in test_value\n    self.assertEqual(workflow.value(), 1)\n  File \"src/workflow.py\", line 4, in value\n    raise ValueError(\"bad\")\nValueError: bad\nERROR: test_value",
        )
        .contains(&"generated-test conflict evidence".to_string())
        && language_public_state_assertions(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_transition\n    workflow = workflow.advance()\n    self.assertEqual(workflow.status, \"COMPLETE\")\nAssertionError: 'PENDING' != 'COMPLETE'",
        )
        .contains(&"workflow.status from workflow = workflow.advance()".to_string())
        && language_public_state_assertion_observations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_transition\n    workflow = workflow.advance()\n    self.assertEqual(workflow.status, \"COMPLETE\")\nAssertionError: 'PENDING' != 'COMPLETE'",
        )
        .contains(&"`workflow.status from workflow = workflow.advance()` expected `\"COMPLETE\"` but observed `'PENDING'`".to_string())
        && language_public_state_terminal_transition_obligations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_transition\n    self.assertEqual(workflow.status, \"COMPLETE\")",
        )
        .contains(&"workflow.status terminal transition to \"COMPLETE\"".to_string())
        && language_public_missing_attributes(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_api\n    workflow.ready()\nAttributeError: 'Workflow' object has no attribute 'ready'",
        )
        .contains(&"Workflow.ready".to_string())
        && language_public_missing_method_attributes(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_api\n    workflow.ready()\nAttributeError: 'Workflow' object has no attribute 'ready'",
        )
        .first()
        .is_some_and(|method| method.attribute == "Workflow.ready" && method.call_site == "workflow.ready()")
        && language_public_class_member_repair_observations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_enum\n    self.assertEqual(Color.RED.value, 1)\nAttributeError: type object 'Color' has no attribute 'RED'. Did you mean: 'red'?",
        )
        .first()
        .is_some_and(|observation| observation.contains("Color.RED") && observation.contains("Color.red"))
        && language_public_constructor_signature_mismatch(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 4, in test_ctor\n    Workflow(size=3)\nTypeError: Workflow.__init__() got an unexpected keyword argument 'size'",
        )
        .is_some_and(|mismatch| mismatch.constructor == "Workflow" && mismatch.unexpected_keyword.as_deref() == Some("size"))
        && language_public_callable_signature_mismatch(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 4, in test_call\n    Workflow().configure()\nTypeError: Workflow.configure() missing 1 required positional argument: 'options'",
        )
        .is_some_and(|mismatch| mismatch.callable == "Workflow.configure" && mismatch.missing_arguments.contains(&"options".to_string()))
        && language_public_api_data_model_semantic_obligations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 4, in test_ctor\n    Workflow(size=3)\nTypeError: Workflow.__init__() got an unexpected keyword argument 'size'",
        )
        .contains(&"constructor keyword compatibility for `Workflow` fields (`size`)".to_string())
        && language_public_api_data_model_semantic_obligations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_transition\n    workflow = workflow.advance()\n    self.assertEqual(workflow.status, \"COMPLETE\")\nAssertionError: 'PENDING' != 'COMPLETE'",
        )
        .contains(&"public state assertion compatibility for `workflow.status from workflow = workflow.advance()` expected `\"COMPLETE\"` but observed `'PENDING'`".to_string())
        && language_public_api_data_model_semantic_obligations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_transition\n    workflow = workflow.advance()\n    self.assertEqual(workflow.status, \"COMPLETE\")\nAssertionError: 'PENDING' != 'COMPLETE'",
        )
        .contains(&"caller-visible public state assertion for `workflow.status from workflow = workflow.advance()`".to_string())
        && language_public_method_sibling_obligations(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 8, in test_api\n    value.ready()\nAttributeError: 'int' object has no attribute 'ready'",
        )
        .contains(&"collection element shape defect `int.ready`".to_string())
        && language_public_exception_mismatch(
            "Traceback (most recent call last):\n  File \"tests/test_workflow.py\", line 4, in test_error\n    workflow.divide(1, 0)\n  File \"workflow.py\", line 9, in divide\n    raise ZeroDivisionError('x')\nZeroDivisionError: x",
        )
        .is_some_and(|mismatch| mismatch.actual_exception == "ZeroDivisionError" && mismatch.source_site.as_deref() == Some("workflow.py"))
        && language_failure_labels_from_summary(
            "FAIL: test_cli (tests.test_workflow.TestWorkflow.test_cli)\nsrc/workflow.spec.ts ... ERROR",
            4,
        ) == vec!["test_cli".to_string(), "src/workflow.spec.ts".to_string()]
        && language_failure_paths_from_summary(
            "Traceback (most recent call last):\n  File \"C:/Python313/Lib/unittest/case.py\", line 1\n  File \"tests/test_workflow.py\", line 7\nImportError: cannot import name 'render' from 'workflow' (C:/workspace/workflow.py)",
        ) == vec!["C:/workspace/workflow.py".to_string(), "tests/test_workflow.py".to_string()]
        && language_failure_requirement_contexts_from_sources(
            &["test_contract".to_string()],
            &["# REQ-42\nclass TestWorkflow(unittest.TestCase):\n    def test_contract(self):\n        pass\n".to_string()],
            4,
        ) == vec!["test_contract -> REQ-42".to_string()]
        && language_failure_assertion_contexts_from_sources(
            "verification failed: test_contract; latest detail: self.assertTrue(observed)",
            &["test_contract".to_string()],
            &["class TestWorkflow(unittest.TestCase):\n    def test_contract(self):\n        observed = workflow.ready()\n        self.assertTrue(observed)\n".to_string()],
            4,
        )
        .first()
        .is_some_and(|context| context.contains("observed = workflow.ready()"))
        && language_source_targets_from_text_handles_line_column_call_site_fixture_passes()
        && language_generated_test_local_binding_contradictions(
            &["test_public_tuple_contract".to_string()],
            &[(
                "test_workflow.py".to_string(),
                "class TestGenerated(unittest.TestCase):\n    def test_public_tuple_contract(self):\n        first, marker, first = workflow.public_tuple()\n        self.assertEqual(first, \"alpha\")\n"
                    .to_string(),
            )],
            "Traceback (most recent call last):\n  File \"test_workflow.py\", line 7, in test_public_tuple_contract\n    self.assertEqual(first, \"alpha\")",
        )
        .first()
        .is_some_and(|item| item.test_target == "test_workflow.py" && item.identifier == "first")
}

pub(crate) fn language_evidence_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    let source = include_str!("language_evidence.rs").to_ascii_lowercase();
    let forbidden = [
        ["comp", "onent"].concat(),
        ["wid", "get"].concat(),
        ["calc", "ulator"].concat(),
    ];
    !forbidden.iter().any(|item| source.contains(item)) && source.contains("workflow")
}

#[cfg(test)]
mod tests {
    #[test]
    fn language_evidence_adapter_registry_fixture_passes() {
        assert!(super::language_evidence_adapter_registry_fixture_passes());
    }

    #[test]
    fn language_source_targets_from_text_handles_line_column_call_site() {
        assert!(
            super::language_source_targets_from_text_handles_line_column_call_site_fixture_passes()
        );
    }

    #[test]
    fn language_evidence_fixtures_are_workflow_neutral() {
        assert!(super::language_evidence_fixtures_are_workflow_neutral_fixture_passes());
    }
}
