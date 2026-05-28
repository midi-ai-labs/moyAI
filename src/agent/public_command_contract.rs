use std::hash::{Hash, Hasher};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};

use crate::agent::content_shape_contract::python_source_for_test_target;
use crate::edit::{PatchOperation, PatchParser};
use crate::protocol::OperationIntent;
use crate::tool::ToolResult;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PublicCommandObligation {
    command: String,
    script_path: String,
    argv_after_script: Vec<String>,
    output_observation_alternatives: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PublicCommandContractIssueKind {
    CoverageMissing,
    EncodingMissing,
    TimeoutMissing,
    CaptureMissing,
    CoverageAndEncodingMissing,
    CoverageAndTimeoutMissing,
    EncodingAndTimeoutMissing,
    CoverageEncodingAndTimeoutMissing,
}

impl PublicCommandContractIssueKind {
    fn from_parts(
        missing: &[PublicCommandObligation],
        encoding_issues: &[String],
        timeout_issues: &[String],
        capture_issues: &[String],
    ) -> Self {
        if !capture_issues.is_empty() {
            return Self::CaptureMissing;
        }
        match (
            missing.is_empty(),
            encoding_issues.is_empty(),
            timeout_issues.is_empty(),
        ) {
            (false, false, false) => Self::CoverageEncodingAndTimeoutMissing,
            (false, false, true) => Self::CoverageAndEncodingMissing,
            (false, true, false) => Self::CoverageAndTimeoutMissing,
            (false, true, true) => Self::CoverageMissing,
            (true, false, false) => Self::EncodingAndTimeoutMissing,
            (true, false, true) => Self::EncodingMissing,
            (true, true, false) => Self::TimeoutMissing,
            (true, true, true) => Self::CoverageMissing,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::CoverageMissing => "public_command_contract_coverage_missing",
            Self::EncodingMissing => "public_command_contract_encoding_missing",
            Self::TimeoutMissing => "public_command_contract_subprocess_timeout_missing",
            Self::CaptureMissing => "public_command_contract_subprocess_output_capture_missing",
            Self::CoverageAndEncodingMissing => {
                "public_command_contract_coverage_and_encoding_missing"
            }
            Self::CoverageAndTimeoutMissing => {
                "public_command_contract_coverage_and_subprocess_timeout_missing"
            }
            Self::EncodingAndTimeoutMissing => {
                "public_command_contract_encoding_and_subprocess_timeout_missing"
            }
            Self::CoverageEncodingAndTimeoutMissing => {
                "public_command_contract_coverage_encoding_and_subprocess_timeout_missing"
            }
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::CoverageMissing => "Public command contract coverage missing",
            Self::EncodingMissing => "Public command contract encoding missing",
            Self::TimeoutMissing => "Public command subprocess timeout missing",
            Self::CaptureMissing => "Public command subprocess output capture missing",
            Self::CoverageAndEncodingMissing => {
                "Public command contract coverage and encoding missing"
            }
            Self::CoverageAndTimeoutMissing => {
                "Public command contract coverage and subprocess timeout missing"
            }
            Self::EncodingAndTimeoutMissing => {
                "Public command contract encoding and subprocess timeout missing"
            }
            Self::CoverageEncodingAndTimeoutMissing => {
                "Public command contract coverage, encoding, and subprocess timeout missing"
            }
        }
    }
}

pub(crate) fn public_command_contract_result(
    tool_name: &str,
    arguments: &Value,
    latest_user_text: Option<&str>,
    workspace_root: Option<&Utf8Path>,
) -> Option<ToolResult> {
    if !matches!(tool_name, "write" | "apply_patch") {
        return None;
    }
    let candidate = public_command_candidate_from_tool(tool_name, arguments, workspace_root)?;
    let target_contract = python_source_for_test_target(&candidate.target)?;
    let source_name = target_contract.source_path.replace('\\', "/");
    let obligations = latest_user_text
        .map(public_command_obligations_from_text)
        .unwrap_or_default();
    let relevant = obligations
        .into_iter()
        .filter(|obligation| obligation.script_path == source_name)
        .collect::<Vec<_>>();
    let missing = relevant
        .iter()
        .filter(|obligation| !candidate_covers_public_command(&candidate.content, obligation))
        .cloned()
        .collect::<Vec<_>>();
    let encoding_issues = if relevant.is_empty() {
        Vec::new()
    } else {
        public_command_subprocess_encoding_issues(&candidate.content)
    };
    let timeout_issues = public_command_subprocess_timeout_issues(&candidate.content);
    let capture_issues = public_command_subprocess_output_capture_issues(&candidate.content);
    if missing.is_empty()
        && encoding_issues.is_empty()
        && timeout_issues.is_empty()
        && capture_issues.is_empty()
    {
        return None;
    }
    Some(public_command_contract_tool_result(
        tool_name,
        arguments,
        &candidate,
        &missing,
        &encoding_issues,
        &timeout_issues,
        &capture_issues,
    ))
}

pub(crate) fn public_command_contract_key(result: &ToolResult) -> String {
    let target = result
        .metadata
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let issue_kind = result
        .metadata
        .get("public_command_contract_issue_kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let missing = result
        .metadata
        .get("missing_public_commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let encoding_issues = result
        .metadata
        .get("encoding_contract_issues")
        .and_then(Value::as_array)
        .map(|issues| {
            issues
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let timeout_issues = result
        .metadata
        .get("subprocess_timeout_contract_issues")
        .and_then(Value::as_array)
        .map(|issues| {
            issues
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let capture_issues = result
        .metadata
        .get("subprocess_output_capture_contract_issues")
        .and_then(Value::as_array)
        .map(|issues| {
            issues
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let missing_observations = result
        .metadata
        .get("missing_public_command_observations")
        .and_then(Value::as_array)
        .map(|observations| {
            observations
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    format!(
        "public_command_contract:{target}:{issue_kind}:{missing}:{missing_observations}:{encoding_issues}:{timeout_issues}:{capture_issues}"
    )
}

pub(crate) fn public_command_contract_terminal_message(
    result: &ToolResult,
    correction_count: usize,
) -> String {
    let target = result
        .metadata
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("test target");
    match result
        .metadata
        .get("public_command_contract_issue_kind")
        .and_then(Value::as_str)
    {
        Some("public_command_contract_encoding_missing") => {
            format!(
                "Public command contract encoding was missing {correction_count} time(s). Runtime stopped before accepting generated tests that decode child command output as UTF-8 without explicit child output encoding authority. Target: {target}."
            )
        }
        Some("public_command_contract_subprocess_timeout_missing") => {
            format!(
                "Public command subprocess timeout was missing {correction_count} time(s). Runtime stopped before accepting generated tests that can block indefinitely on a child process. Target: {target}."
            )
        }
        Some("public_command_contract_subprocess_output_capture_missing") => {
            format!(
                "Public command subprocess output capture was missing {correction_count} time(s). Runtime stopped before accepting generated tests that assert CompletedProcess stdout/stderr without capturing those streams. Target: {target}."
            )
        }
        Some(issue) if issue.contains("subprocess_timeout_missing") => {
            format!(
                "Public command subprocess contract was incomplete {correction_count} time(s). Runtime stopped before accepting generated tests that can block indefinitely on a child process. Target: {target}.{}",
                missing_observation_terminal_suffix(result)
            )
        }
        Some("public_command_contract_coverage_and_encoding_missing") => {
            format!(
                "Public command contract coverage and encoding were incomplete {correction_count} time(s). Runtime stopped before accepting generated tests that both omit prompt-visible public command examples and lack explicit child output encoding authority. Target: {target}.{}",
                missing_observation_terminal_suffix(result)
            )
        }
        _ => {
            format!(
                "Public command contract coverage was missing {correction_count} time(s). Runtime stopped before accepting generated tests that omit prompt-visible public command examples. Target: {target}.{}",
                missing_observation_terminal_suffix(result)
            )
        }
    }
}

fn missing_observation_terminal_suffix(result: &ToolResult) -> String {
    let observations = missing_public_command_observation_descriptions(result);
    if observations.is_empty() {
        String::new()
    } else {
        format!(
            " Missing output observation(s): {}.",
            observations.join("; ")
        )
    }
}

fn missing_public_command_observation_descriptions(result: &ToolResult) -> Vec<String> {
    result
        .metadata
        .get("missing_public_command_observations")
        .and_then(Value::as_array)
        .map(|observations| {
            observations
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn public_command_contract_fixture_passes() -> bool {
    let authority = "Update the implementation and tests. CLI must support `python tool.py render sample`, `python tool.py convert input.txt`, `python tool.py status`, and `python tool.py missing input.txt` as public command forms with documented exit codes.";
    let bad = json!({
        "path": "test_tool.py",
        "content": r#"
import unittest
from tool import run

class TestTool(unittest.TestCase):
    def test_run(self):
        self.assertEqual(run("status"), "ready")
"#
    });
    let rejected = public_command_contract_result("write", &bad, Some(authority), None);
    let Some(rejected) = rejected else {
        return false;
    };
    let good = json!({
        "path": "test_tool.py",
        "content": r#"
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            timeout=10,
        )

    def test_render_cli(self):
        result = self.run_cli("render", "sample")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "rendered")

    def test_convert_cli(self):
        result = self.run_cli("convert", "input.txt")
        self.assertEqual(result.returncode, 0)

    def test_status_cli(self):
        result = self.run_cli("status")
        self.assertEqual(result.returncode, 0)

    def test_missing_cli(self):
        result = self.run_cli("missing", "input.txt")
        self.assertEqual(result.returncode, 1)
        self.assertTrue(result.stderr or result.stdout)
"#
    });
    let scattered_tokens_without_invocation = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_render_uses_wrong_sample(self):
        result = self.run_cli("render", "other")
        self.assertEqual(result.returncode, 0)
        self.assertIn("sample", result.stdout)
"#
    });
    let usage_only_returncode = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_missing_cli(self):
        result = self.run_cli("missing", "input.txt")
        self.assertEqual(result.returncode, 1)
        self.assertTrue(result.stderr or result.stdout)
"#
    });
    let usage_observed = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_missing_cli(self):
        result = self.run_cli("missing", "input.txt")
        self.assertEqual(result.returncode, 1)
        self.assertIn("usage", result.stderr.lower())
"#
    });
    let template_helper_covered = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["python", "-X", "utf8", "tool.py"] + args,
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_action_target_template(self):
        result = self.run_cli(["create", "sample.txt"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("created", result.stdout)

    def test_fixed_token_template(self):
        result = self.run_cli(["convert", "json", "sample.txt"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("converted", result.stdout)

    def test_concrete_command(self):
        result = self.run_cli(["status"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("ready", result.stdout)
"#
    });
    let template_helper_missing = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["python", "-X", "utf8", "tool.py"] + args,
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_concrete_command_only(self):
        result = self.run_cli(["status"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("ready", result.stdout)
"#
    });
    let template_usage_returncode_only = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["python", "-X", "utf8", "tool.py"] + args,
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_template_usage(self):
        result = self.run_cli(["missing", "sample.txt"])
        self.assertEqual(result.returncode, 1)
        self.assertTrue(result.stderr or result.stdout)
"#
    });
    let template_usage_observed = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["python", "-X", "utf8", "tool.py"] + args,
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_template_usage(self):
        result = self.run_cli(["missing", "sample.txt"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("usage", result.stderr.lower())
"#
    });
    public_command_contract_result("write", &good, Some(authority), None).is_none()
        && public_command_contract_result(
            "write",
            &scattered_tokens_without_invocation,
            Some("CLI must support `python tool.py render sample`."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("missing_public_commands")
                .and_then(Value::as_array)
                .is_some_and(|commands| {
                    commands
                        .iter()
                        .any(|command| command.as_str() == Some("python tool.py render sample"))
                })
        })
        && rejected.recorded_changes.is_empty()
        && rejected
            .metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(false)
        && rejected
            .metadata
            .get("missing_public_commands")
            .and_then(Value::as_array)
            .is_some_and(|commands| {
                commands
                    .iter()
                    .any(|command| command.as_str() == Some("python tool.py render sample"))
            })
        && public_command_contract_result(
            "write",
            &json!({
                "path": "test_tool.py",
                "content": r#"
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            timeout=10,
        )

    def test_status_cli(self):
        result = self.run_cli("status")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "ready")
"#
            }),
            Some("CLI must support `python tool.py status`."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("encoding_contract_issues")
                .and_then(Value::as_array)
                .is_some_and(|issues| !issues.is_empty())
                && result
                    .metadata
                    .get("missing_public_commands")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
                && result
                    .metadata
                    .get("public_command_contract_issue_kind")
                    .and_then(Value::as_str)
                    == Some("public_command_contract_encoding_missing")
                && result
                    .metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("public_command_contract_encoding_missing")
                && result.title == "Public command contract encoding missing"
                && public_command_contract_terminal_message(&result, 3)
                    .contains("Public command contract encoding was missing 3 time(s)")
        })
        && public_command_contract_result(
            "write",
            &json!({
                "path": "test_tool.py",
                "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def test_status_cli(self):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        result = subprocess.run(
            [sys.executable, "tool.py", "status"],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
        )
        self.assertEqual(result.returncode, 0)
"#
            }),
            Some("Create a CLI tool and tests."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("public_command_contract_issue_kind")
                .and_then(Value::as_str)
                == Some("public_command_contract_subprocess_timeout_missing")
                && result
                    .metadata
                    .get("subprocess_timeout_contract_issues")
                    .and_then(Value::as_array)
                    .is_some_and(|issues| !issues.is_empty())
                && result.output_text.contains("bounded timeout")
                && public_command_contract_terminal_message(&result, 2)
                    .contains("subprocess timeout was missing 2 time(s)")
        })
        && public_command_contract_result(
            "write",
            &json!({
                "path": "test_tool.py",
                "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def test_status_cli(self):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        result = subprocess.run(
            [sys.executable, "tool.py", "status"],
            text=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("ready", result.stdout)
"#
            }),
            Some("CLI must support `python tool.py status`."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("public_command_contract_issue_kind")
                .and_then(Value::as_str)
                == Some("public_command_contract_subprocess_output_capture_missing")
                && result
                    .metadata
                    .get("subprocess_output_capture_contract_issues")
                    .and_then(Value::as_array)
                    .is_some_and(|issues| !issues.is_empty())
                && result.output_text.contains("CompletedProcess.stdout")
                && public_command_contract_terminal_message(&result, 2)
                    .contains("subprocess output capture was missing 2 time(s)")
        })
        && public_command_contract_result(
            "write",
            &json!({
                "path": "test_tool.py",
                "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            [sys.executable, "tool.py", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_status_cli(self):
        result = self.run_cli("status")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "ready")
"#
            }),
            Some("CLI must support `python tool.py status`."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &usage_only_returncode,
            Some("CLI must treat `python tool.py missing input.txt` as a usage error with exit code 1."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("missing_public_commands")
                .and_then(Value::as_array)
                .is_some_and(|commands| {
                    commands
                        .iter()
                        .any(|command| command.as_str() == Some("python tool.py missing input.txt"))
                })
                && result.output_text.contains("usage | 使い方 | 使用方法")
                && result
                    .metadata
                    .pointer("/tool_feedback_envelope/missing_public_command_observations")
                    .and_then(Value::as_array)
                    .is_some_and(|observations| {
                        observations
                            .iter()
                            .any(|observation| {
                                observation
                                    .as_str()
                                    .is_some_and(|value| value.contains("usage | 使い方 | 使用方法"))
                            })
                    })
                && public_command_contract_key(&result).contains("usage | 使い方 | 使用方法")
                && public_command_contract_terminal_message(&result, 3)
                    .contains("usage | 使い方 | 使用方法")
        })
        && public_command_contract_result(
            "write",
            &usage_observed,
            Some("CLI must treat `python tool.py missing input.txt` as a usage error with exit code 1."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &template_helper_covered,
            Some("Public command forms include `python tool.py <action> <target>`, `python tool.py convert <format> <target>`, and `python tool.py status`."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &template_helper_missing,
            Some("Public command forms include `python tool.py <action> <target>`, `python tool.py convert <format> <target>`, and `python tool.py status`."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("missing_public_commands")
                .and_then(Value::as_array)
                .is_some_and(|commands| {
                    commands.iter().any(|command| {
                        command.as_str() == Some("python tool.py <action> <target>")
                    }) && commands.iter().any(|command| {
                        command.as_str()
                            == Some("python tool.py convert <format> <target>")
                    })
                })
        })
        && public_command_contract_result(
            "write",
            &template_usage_returncode_only,
            Some("Public command form `python tool.py <action> <target>` reports a usage error with exit code 1."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("missing_public_command_observations")
                .and_then(Value::as_array)
                .is_some_and(|observations| !observations.is_empty())
        })
        && public_command_contract_result(
            "write",
            &template_usage_observed,
            Some("Public command form `python tool.py <action> <target>` reports a usage error with exit code 1."),
            None,
        )
        .is_none()
}

pub fn public_command_contract_apply_patch_uses_post_patch_content_fixture_passes() -> bool {
    let authority = "Update tests. CLI must support `python tool.py status` and `python tool.py render sample`.";
    let root = match temp_contract_workspace() {
        Some(root) => root,
        None => return false,
    };
    let test_path = root.join("test_tool.py");
    let original = r#"import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run([sys.executable, "tool.py", *args], text=True, capture_output=True, timeout=10)

    def test_status_cli(self):
        result = self.run_cli("status")
        self.assertEqual(result.returncode, 0)
"#;
    if std::fs::create_dir_all(root.as_std_path()).is_err()
        || std::fs::write(test_path.as_std_path(), original).is_err()
    {
        return false;
    }

    let incremental_patch = json!({
        "patch_text": r#"*** Begin Patch
*** Update File: test_tool.py
@@
     def test_status_cli(self):
         result = self.run_cli("status")
         self.assertEqual(result.returncode, 0)
+
+    def test_render_cli(self):
+        result = self.run_cli("render", "sample")
+        self.assertEqual(result.returncode, 0)
+        self.assertEqual(result.stdout.strip(), "rendered")
*** End Patch"#
    });
    let incremental_passes = public_command_contract_result(
        "apply_patch",
        &incremental_patch,
        Some(authority),
        Some(root.as_path()),
    )
    .is_none();

    let deleting_patch = json!({
        "patch_text": r#"*** Begin Patch
*** Update File: test_tool.py
@@
-    def test_status_cli(self):
-        result = self.run_cli("status")
-        self.assertEqual(result.returncode, 0)
*** End Patch"#
    });
    let deleting_rejected = public_command_contract_result(
        "apply_patch",
        &deleting_patch,
        Some(authority),
        Some(root.as_path()),
    )
    .is_some_and(|result| {
        result
            .metadata
            .get("missing_public_commands")
            .and_then(Value::as_array)
            .is_some_and(|commands| {
                commands
                    .iter()
                    .any(|command| command.as_str() == Some("python tool.py status"))
            })
    });

    let _ = std::fs::remove_dir_all(root.as_std_path());
    incremental_passes && deleting_rejected
}

fn public_command_obligations_from_text(text: &str) -> Vec<PublicCommandObligation> {
    text.lines()
        .flat_map(|line| {
            backtick_spans(line)
                .into_iter()
                .filter_map(|span| public_command_obligation_from_command(&span, line))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn backtick_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut in_span = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if in_span {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    spans.push(trimmed.to_string());
                }
                current.clear();
            }
            in_span = !in_span;
            continue;
        }
        if in_span {
            current.push(ch);
        }
    }
    spans
}

fn public_command_obligation_from_command(
    command: &str,
    context: &str,
) -> Option<PublicCommandObligation> {
    let parts = command.split_whitespace().collect::<Vec<_>>();
    let first = parts.first()?.to_ascii_lowercase();
    if !matches!(first.as_str(), "python" | "python3" | "py") {
        return None;
    }
    let mut script_index = None;
    let mut index = 1usize;
    while index < parts.len() {
        let token = parts[index];
        if token.ends_with(".py") {
            script_index = Some(index);
            break;
        }
        if matches!(token, "-X" | "-m" | "-c") {
            index += 2;
        } else {
            index += 1;
        }
    }
    let script_index = script_index?;
    let script_path = parts[script_index].replace('\\', "/");
    let argv_after_script = parts
        .iter()
        .skip(script_index + 1)
        .map(|part| part.trim_matches('"').trim_matches('\'').to_string())
        .collect::<Vec<_>>();
    if argv_after_script.is_empty() {
        return None;
    }
    Some(PublicCommandObligation {
        command: command.to_string(),
        script_path,
        argv_after_script,
        output_observation_alternatives: output_observation_alternatives_from_context(context),
    })
}

fn output_observation_alternatives_from_context(context: &str) -> Vec<String> {
    let lower = context.to_ascii_lowercase();
    let mut alternatives = Vec::new();
    if lower.contains("usage") || context.contains("使用方法") || context.contains("使い方")
    {
        alternatives.extend(
            ["usage", "使用方法", "使い方"]
                .into_iter()
                .map(str::to_string),
        );
    } else if lower.contains("undefined")
        || lower.contains("unsupported")
        || context.contains("未定義")
        || context.contains("未対応")
    {
        alternatives.extend(
            ["undefined", "unsupported", "未定義", "未対応"]
                .into_iter()
                .map(str::to_string),
        );
    }
    alternatives.sort();
    alternatives.dedup();
    alternatives
}

#[derive(Debug, Clone)]
struct PublicCommandCandidate {
    target: String,
    content: String,
}

fn public_command_candidate_from_tool(
    tool_name: &str,
    arguments: &Value,
    workspace_root: Option<&Utf8Path>,
) -> Option<PublicCommandCandidate> {
    match tool_name {
        "write" => {
            let target = arguments.get("path").and_then(Value::as_str)?;
            let content = arguments.get("content").and_then(Value::as_str)?;
            Some(PublicCommandCandidate {
                target: normalize_path(target),
                content: content.to_string(),
            })
        }
        "apply_patch" => {
            let patch_text = arguments.get("patch_text").and_then(Value::as_str)?;
            public_command_candidate_from_patch(patch_text, workspace_root)
        }
        _ => None,
    }
}

fn public_command_candidate_from_patch(
    patch_text: &str,
    workspace_root: Option<&Utf8Path>,
) -> Option<PublicCommandCandidate> {
    let operations = PatchParser::parse(patch_text).ok()?;
    for operation in operations {
        match operation {
            PatchOperation::Add { path, contents } => {
                let target = normalize_path(path.as_str());
                if python_source_for_test_target(&target).is_some() {
                    return Some(PublicCommandCandidate {
                        target,
                        content: contents,
                    });
                }
            }
            PatchOperation::Update {
                path,
                hunks,
                move_to,
            } => {
                let source = normalize_path(path.as_str());
                let target = move_to
                    .as_ref()
                    .map(|path| normalize_path(path.as_str()))
                    .unwrap_or_else(|| source.clone());
                if python_source_for_test_target(&target).is_none() {
                    continue;
                }
                let root = workspace_root?;
                let original =
                    std::fs::read_to_string(root.join(source.as_str()).as_std_path()).ok()?;
                let patched = PatchParser::apply_to_text(&original, &hunks).ok()?;
                return Some(PublicCommandCandidate {
                    target,
                    content: patched,
                });
            }
            PatchOperation::Delete { path } => {
                let target = normalize_path(path.as_str());
                if python_source_for_test_target(&target).is_some() {
                    return Some(PublicCommandCandidate {
                        target,
                        content: String::new(),
                    });
                }
            }
        }
    }
    None
}

fn candidate_covers_public_command(content: &str, obligation: &PublicCommandObligation) -> bool {
    let lower = content.to_ascii_lowercase();
    if !lower.contains("subprocess")
        || !lower.contains("returncode")
        || !(lower.contains("stdout") || lower.contains("stderr"))
    {
        return false;
    }
    if !contains_command_token(content, &obligation.script_path) {
        return false;
    }
    let observed_invocations =
        observed_public_command_invocations(content, &obligation.script_path);
    let argv_covered = if obligation
        .argv_after_script
        .iter()
        .any(|token| is_public_command_placeholder(token))
    {
        observed_invocations
            .iter()
            .any(|argv| argv_matches_template(argv, &obligation.argv_after_script))
    } else {
        observed_invocations
            .iter()
            .any(|argv| argv == &obligation.argv_after_script)
    };
    argv_covered
        && candidate_asserts_output_observation(
            content,
            &obligation.output_observation_alternatives,
        )
}

fn observed_public_command_invocations(content: &str, script_path: &str) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    for values in string_list_literals(content) {
        if let Some(script_index) = values
            .iter()
            .position(|value| normalized_token_eq(value, script_path))
        {
            let argv = values
                .iter()
                .skip(script_index + 1)
                .filter(|value| !value.is_empty())
                .cloned()
                .collect::<Vec<_>>();
            if !argv.is_empty() {
                push_unique_invocation(&mut invocations, argv);
            }
        } else if looks_like_public_command_argv(&values) {
            push_unique_invocation(&mut invocations, values);
        }
    }
    for argv in run_helper_call_arg_vectors(content) {
        if looks_like_public_command_argv(&argv) {
            push_unique_invocation(&mut invocations, argv);
        }
    }
    invocations
}

fn push_unique_invocation(invocations: &mut Vec<Vec<String>>, argv: Vec<String>) {
    if !invocations.iter().any(|existing| existing == &argv) {
        invocations.push(argv);
    }
}

fn argv_matches_template(argv: &[String], template: &[String]) -> bool {
    argv.len() == template.len()
        && argv.iter().zip(template).all(|(observed, expected)| {
            if is_public_command_placeholder(expected) {
                !observed.trim().is_empty() && !is_public_command_placeholder(observed)
            } else {
                observed == expected
            }
        })
}

fn is_public_command_placeholder(token: &str) -> bool {
    let trimmed = token.trim();
    trimmed.len() >= 3 && trimmed.starts_with('<') && trimmed.ends_with('>')
}

fn normalized_token_eq(left: &str, right: &str) -> bool {
    left.replace('\\', "/") == right.replace('\\', "/")
}

fn looks_like_public_command_argv(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 8
        && values.iter().all(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.ends_with(".py")
                && !trimmed.eq_ignore_ascii_case("python")
                && !trimmed.eq_ignore_ascii_case("python3")
                && !trimmed.eq_ignore_ascii_case("py")
                && !trimmed.eq_ignore_ascii_case("utf-8")
                && !trimmed.eq_ignore_ascii_case("utf8")
        })
}

fn string_list_literals(content: &str) -> Vec<Vec<String>> {
    let bytes = content.as_bytes();
    let mut lists = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'[' {
            index += 1;
            continue;
        }
        if let Some((end, inside)) = bracket_body(content, index, b'[', b']') {
            let values = string_literals(inside);
            if !values.is_empty() {
                lists.push(values);
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    lists
}

fn run_helper_call_arg_vectors(content: &str) -> Vec<Vec<String>> {
    let mut vectors = Vec::new();
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("subprocess.run") || !(lower.contains("run") || lower.contains("cli")) {
            continue;
        }
        let mut search_from = 0usize;
        while let Some(relative_open) = line[search_from..].find('(') {
            let open = search_from + relative_open;
            let name_start = line[..open]
                .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
                .map(|position| position + 1)
                .unwrap_or(0);
            let callable = line[name_start..open].to_ascii_lowercase();
            search_from = open + 1;
            if !(callable.contains("run") || callable.contains("cli")) {
                continue;
            }
            if let Some((close, body)) = bracket_body(line, open, b'(', b')') {
                let trimmed = body.trim();
                if trimmed.starts_with('[') {
                    if let Some((_, list_body)) = bracket_body(trimmed, 0, b'[', b']') {
                        let values = string_literals(list_body);
                        if !values.is_empty() {
                            vectors.push(values);
                        }
                    }
                } else {
                    let values = string_literals(trimmed);
                    if !values.is_empty() {
                        vectors.push(values);
                    }
                }
                search_from = close + 1;
            }
        }
    }
    vectors
}

fn bracket_body(text: &str, open: usize, open_ch: u8, close_ch: u8) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    if bytes.get(open).copied()? != open_ch {
        return None;
    }
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for index in open..bytes.len() {
        let byte = bytes[index];
        if let Some(quote_byte) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote_byte {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            value if value == open_ch => depth += 1,
            value if value == close_ch => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((index, &text[open + 1..index]));
                }
            }
            _ => {}
        }
    }
    None
}

fn string_literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut values = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let quote = bytes[index];
        if quote != b'\'' && quote != b'"' {
            index += 1;
            continue;
        }
        let start = index + 1;
        index = start;
        let mut escaped = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                values.push(text[start..index].to_string());
                index += 1;
                break;
            }
            index += 1;
        }
    }
    values
}

fn candidate_asserts_output_observation(content: &str, alternatives: &[String]) -> bool {
    if alternatives.is_empty() {
        return true;
    }
    let lower = content.to_ascii_lowercase();
    if !(lower.contains("stdout") || lower.contains("stderr")) {
        return false;
    }
    content.lines().any(|line| {
        let line_lower = line.to_ascii_lowercase();
        line_lower.contains("assert")
            && alternatives.iter().any(|alternative| {
                line_lower.contains(&alternative.to_ascii_lowercase()) || line.contains(alternative)
            })
    })
}

fn public_command_subprocess_encoding_issues(content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    if !lower.contains("subprocess.run") || !lower.contains("encoding=\"utf-8\"") {
        return Vec::new();
    }
    let child_env_utf8 = lower.contains("pythonutf8") || lower.contains("pythonioencoding");
    if child_env_utf8 {
        return Vec::new();
    }
    vec![
        "subprocess decodes command output as utf-8 but does not pass an explicit UTF-8 environment to the child command".to_string(),
    ]
}

fn public_command_subprocess_timeout_issues(content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    if !lower.contains("subprocess.run") {
        return Vec::new();
    }
    let invocations = subprocess_run_invocation_texts(content);
    if invocations.is_empty() {
        if lower.contains("timeout=") {
            return Vec::new();
        }
        return vec![
            "subprocess.run starts a child command but no bounded timeout argument was found"
                .to_string(),
        ];
    }
    let missing = invocations
        .iter()
        .filter(|invocation| !invocation.to_ascii_lowercase().contains("timeout="))
        .count();
    if missing == 0 {
        Vec::new()
    } else {
        vec![format!(
            "{missing} subprocess.run invocation(s) start child commands without a bounded timeout argument"
        )]
    }
}

fn public_command_subprocess_output_capture_issues(content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    if !lower.contains("subprocess.run")
        || !(lower.contains(".stdout") || lower.contains(".stderr"))
    {
        return Vec::new();
    }
    let needs_stdout = lower.contains(".stdout");
    let needs_stderr = lower.contains(".stderr");
    let invocations = subprocess_run_invocation_texts(content);
    if invocations.is_empty() {
        return Vec::new();
    }
    let missing = invocations
        .iter()
        .filter_map(|invocation| {
            let compact = invocation
                .to_ascii_lowercase()
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            let captures_both = compact.contains("capture_output=true");
            let captures_stdout = captures_both
                || compact.contains("stdout=subprocess.pipe")
                || compact.contains("stdout=pipe");
            let captures_stderr = captures_both
                || compact.contains("stderr=subprocess.pipe")
                || compact.contains("stderr=pipe");
            let mut missing_streams = Vec::new();
            if needs_stdout && !captures_stdout {
                missing_streams.push("stdout");
            }
            if needs_stderr && !captures_stderr {
                missing_streams.push("stderr");
            }
            (!missing_streams.is_empty()).then(|| missing_streams.join("/"))
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "{} subprocess.run invocation(s) return CompletedProcess output inspected by assertions without capturing required stream(s): {}",
            missing.len(),
            missing.join(", ")
        )]
    }
}

fn subprocess_run_invocation_texts(content: &str) -> Vec<String> {
    let mut invocations = Vec::new();
    let needle = "subprocess.run";
    let mut search_from = 0;
    while let Some(relative) = content[search_from..].find(needle) {
        let start = search_from + relative;
        let Some(open_relative) = content[start..].find('(') else {
            break;
        };
        let open = start + open_relative;
        let Some(end) = matching_paren_end(content, open) else {
            break;
        };
        invocations.push(content[start..=end].to_string());
        search_from = end + 1;
    }
    invocations
}

fn matching_paren_end(content: &str, open: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == active_quote {
                quote = None;
            }
            continue;
        }
        match *byte {
            b'\'' | b'"' => quote = Some(*byte),
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn contains_command_token(content: &str, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    content.contains(token)
}

fn normalize_path(value: &str) -> String {
    Utf8Path::new(value).as_str().replace('\\', "/")
}

fn temp_contract_workspace() -> Option<Utf8PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "moyai-public-command-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    Utf8PathBuf::from_path_buf(path).ok()
}

fn public_command_contract_tool_result(
    tool_name: &str,
    arguments: &Value,
    candidate: &PublicCommandCandidate,
    missing: &[PublicCommandObligation],
    encoding_issues: &[String],
    timeout_issues: &[String],
    capture_issues: &[String],
) -> ToolResult {
    let missing_commands = missing
        .iter()
        .map(|obligation| obligation.command.clone())
        .collect::<Vec<_>>();
    let missing_observations = missing
        .iter()
        .filter(|obligation| !obligation.output_observation_alternatives.is_empty())
        .map(|obligation| {
            format!(
                "{} requires output observation containing one of: {}",
                obligation.command,
                obligation.output_observation_alternatives.join(" | ")
            )
        })
        .collect::<Vec<_>>();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    candidate.target.hash(&mut hasher);
    missing_commands.hash(&mut hasher);
    encoding_issues.hash(&mut hasher);
    timeout_issues.hash(&mut hasher);
    capture_issues.hash(&mut hasher);
    let result_hash = format!("public-command-contract-{:016x}", hasher.finish());
    let commands_display = if missing_commands.is_empty() {
        "none missing".to_string()
    } else {
        missing_commands.join(", ")
    };
    let observations_display = if missing_observations.is_empty() {
        String::new()
    } else {
        format!(
            " Missing output observation(s): {}. These observations are required stdout/stderr content; a generic error assertion does not satisfy them.",
            missing_observations.join("; ")
        )
    };
    let issue_kind = PublicCommandContractIssueKind::from_parts(
        missing,
        encoding_issues,
        timeout_issues,
        capture_issues,
    );
    let mut issue_sentences = Vec::new();
    if !missing_commands.is_empty() {
        issue_sentences.push(format!(
            "`{target}` does not cover prompt-visible public command contract(s): {commands}.{observations} Add subprocess-based tests that execute the exact argv forms, assert return code, and assert the listed stdout/stderr observation.",
            target = candidate.target,
            commands = commands_display,
            observations = observations_display
        ));
    }
    if !encoding_issues.is_empty() {
        issue_sentences.push(format!(
            "`{target}` decodes public command subprocess output as UTF-8 without explicit child output encoding authority. Encoding issue(s): {issues}. Pass an explicit UTF-8 environment such as PYTHONUTF8=1 and PYTHONIOENCODING=utf-8 to the child command.",
            target = candidate.target,
            issues = encoding_issues.join("; ")
        ));
    }
    if !timeout_issues.is_empty() {
        issue_sentences.push(format!(
            "`{target}` starts subprocess child commands without bounded timeout authority. Timeout issue(s): {issues}. Add a finite `timeout=` argument to every generated `subprocess.run(...)` call that can execute a child command, so verification cannot block indefinitely on an interactive or stalled child process.",
            target = candidate.target,
            issues = timeout_issues.join("; ")
        ));
    }
    if !capture_issues.is_empty() {
        issue_sentences.push(format!(
            "`{target}` inspects `CompletedProcess.stdout` or `CompletedProcess.stderr` without capture authority. Capture issue(s): {issues}. Add `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE` to every generated `subprocess.run(...)` call whose output streams are asserted.",
            target = candidate.target,
            issues = capture_issues.join("; ")
        ));
    }
    let output_text = format!(
        "Runtime rejected `{tool_name}` before filesystem side effects because this generated test artifact violates the public command subprocess contract. {} This test artifact cannot satisfy route closeout until the typed subprocess coverage, encoding, timeout, and output-capture contract is coherent.",
        issue_sentences.join(" ")
    );
    ToolResult {
        title: issue_kind.title().to_string(),
        output_text,
        metadata: json!({
            "success": false,
            "public_command_contract_review": true,
            "public_command_contract_coverage": !missing_commands.is_empty(),
            "public_command_contract_issue_kind": issue_kind.as_str(),
            "requested_tool": tool_name,
            "requested_arguments": arguments,
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": issue_kind.as_str(),
            "progress_effect": "no_progress",
            "target": candidate.target,
            "missing_public_commands": missing_commands,
            "missing_public_command_observations": missing_observations,
            "encoding_contract_issues": encoding_issues,
            "subprocess_timeout_contract_issues": timeout_issues,
            "subprocess_output_capture_contract_issues": capture_issues,
            "result_hash": result_hash,
            "tool_feedback_envelope": {
                "kind": issue_kind.as_str(),
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": issue_kind.as_str(),
                "progress_effect": "no_progress",
                "side_effects_applied": false,
                "missing_public_commands": missing_commands,
                "missing_public_command_observations": missing_observations,
                "encoding_contract_issues": encoding_issues,
                "subprocess_timeout_contract_issues": timeout_issues,
                "subprocess_output_capture_contract_issues": capture_issues,
                "result_hash": result_hash
            },
            "terminal_guard_policy": {
                "owner": "tool_lifecycle_runtime",
                "no_progress_guard": true,
                "side_effects_applied": false,
                "terminal_after_repeated_corrections": 3
            }
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}
