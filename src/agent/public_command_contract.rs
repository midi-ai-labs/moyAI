use std::hash::{Hash, Hasher};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};

use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target as classify_language_artifact_target,
};
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicCommandTestTargetContract {
    source_path: Option<String>,
    language: LanguageFamily,
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
            Self::TimeoutMissing => "Public command child-process timeout missing",
            Self::CaptureMissing => "Public command child-process output capture missing",
            Self::CoverageAndEncodingMissing => {
                "Public command contract coverage and encoding missing"
            }
            Self::CoverageAndTimeoutMissing => {
                "Public command contract coverage and child-process timeout missing"
            }
            Self::EncodingAndTimeoutMissing => {
                "Public command contract encoding and child-process timeout missing"
            }
            Self::CoverageEncodingAndTimeoutMissing => {
                "Public command contract coverage, encoding, and child-process timeout missing"
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
    let target_contract = public_command_test_target_contract(&candidate.target)?;
    let source_name = target_contract
        .source_path
        .as_ref()
        .map(|path| path.replace('\\', "/"));
    let obligations = latest_user_text
        .map(public_command_obligations_from_text)
        .unwrap_or_default();
    let source_matched = source_name
        .as_deref()
        .map(|source_name| {
            obligations
                .iter()
                .filter(|obligation| {
                    public_command_subject_matches_source(&obligation.script_path, source_name)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let relevant = if source_matched.is_empty() {
        obligations
    } else {
        source_matched
    };
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
                "Public command child-process timeout was missing {correction_count} time(s). Runtime stopped before accepting generated tests that can block indefinitely on a child process. Target: {target}."
            )
        }
        Some("public_command_contract_subprocess_output_capture_missing") => {
            format!(
                "Public command child-process output capture was missing {correction_count} time(s). Runtime stopped before accepting generated tests that assert child process stdout/stderr without capturing those streams. Target: {target}."
            )
        }
        Some(issue) if issue.contains("subprocess_timeout_missing") => {
            format!(
                "Public command child-process contract was incomplete {correction_count} time(s). Runtime stopped before accepting generated tests that can block indefinitely on a child process. Target: {target}.{}",
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
    let generic_node_bad = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import sys
import unittest

class TestToolCli(unittest.TestCase):
    def test_status_only(self):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        result = subprocess.run(
            [sys.executable, "tool.py", "status"],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0)
"#
    });
    let generic_node_good = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def test_node_render(self):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        result = subprocess.run(
            ["node", "tool.js", "render", "sample"],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("rendered", result.stdout)
"#
    });
    let non_command_support_phrase = json!({
        "path": "test_tool.py",
        "content": r#"
import unittest

class TestToolUi(unittest.TestCase):
    def test_dark_mode_copy(self):
        self.assertTrue("dark mode")
"#
    });
    let generic_js_bad = json!({
        "path": "tool.test.js",
        "content": r#"
import { describe, expect, test } from "vitest";
import { status } from "./tool.js";

describe("tool cli", () => {
  test("status helper", () => {
    expect(status()).toBe("ready");
  });
});
"#
    });
    let generic_js_good = json!({
        "path": "tool.test.js",
        "content": r#"
import { describe, expect, test } from "vitest";
import { spawnSync } from "node:child_process";

describe("tool cli", () => {
  test("render command", () => {
    const result = spawnSync("node", ["tool.js", "render", "sample"], {
      encoding: "utf8",
      timeout: 10000,
    });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("rendered");
  });
});
"#
    });
    let generic_js_no_timeout = json!({
        "path": "tool.test.js",
        "content": r#"
import { describe, expect, test } from "vitest";
import { spawnSync } from "node:child_process";

describe("tool cli", () => {
  test("render command lacks timeout", () => {
    const result = spawnSync("node", ["tool.js", "render", "sample"], {
      encoding: "utf8",
    });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("rendered");
  });
});
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
                    .contains("child-process timeout was missing 2 time(s)")
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
                    .contains("child-process output capture was missing 2 time(s)")
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
                && result
                    .metadata
                    .pointer("/tool_feedback_envelope/required_public_command_assertion_templates")
                    .and_then(Value::as_array)
                    .is_some_and(|templates| {
                        templates.iter().any(|template| {
                            template.as_str().is_some_and(|value| {
                                value.contains("self._run_cli")
                                    && value.contains("proc.returncode")
                                    && value.contains("proc.stdout + proc.stderr")
                                    && value.contains("usage")
                            })
                        })
                    })
                && result
                    .output_text
                    .contains("required_public_command_assertion_templates:")
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
        && public_command_contract_result(
            "write",
            &generic_node_bad,
            Some("CLI must support `node tool.js render sample` and print rendered output."),
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
                        .any(|command| command.as_str() == Some("node tool.js render sample"))
                })
        })
        && public_command_contract_result(
            "write",
            &generic_node_good,
            Some("CLI must support `node tool.js render sample` and print rendered output."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &non_command_support_phrase,
            Some("The command palette should support `dark mode` in the settings panel."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &generic_js_bad,
            Some("CLI must support `node tool.js render sample` and print rendered output."),
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
                        .any(|command| command.as_str() == Some("node tool.js render sample"))
                })
        })
        && public_command_contract_result(
            "write",
            &generic_js_good,
            Some("CLI must support `node tool.js render sample` and print rendered output."),
            None,
        )
        .is_none()
        && public_command_contract_result(
            "write",
            &generic_js_no_timeout,
            Some("CLI must support `node tool.js render sample` and print rendered output."),
            None,
        )
        .is_some_and(|result| {
            result
                .metadata
                .get("subprocess_timeout_contract_issues")
                .and_then(Value::as_array)
                .is_some_and(|issues| !issues.is_empty())
                && result.output_text.contains("child-process")
                && !result
                    .output_text
                    .contains("every generated `subprocess.run(...)` call")
        })
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

pub fn public_command_contract_helper_argv_operator_fixture_passes() -> bool {
    let authority =
        "Update tests. CLI must support `workflow-tool combine draft + review` and print combined.";
    let candidate = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def _run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["workflow-tool", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_combined_public_command(self):
        proc = self._run_cli("combine", "draft", "+", "review")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout.strip(), "combined")
"#
    });
    public_command_contract_result("write", &candidate, Some(authority), None).is_none()
}

pub fn public_command_contract_feedback_projects_typed_missing_coverage_fixture_passes() -> bool {
    let authority =
        "Update tests. CLI must support `workflow-tool combine draft + review` and print combined.";
    let alternate_command = "workflow-tool inspect draft + review";
    let candidate = json!({
        "path": "test_tool.py",
        "content": r#"
import os
import subprocess
import unittest

class TestToolCli(unittest.TestCase):
    def _run_cli(self, *args):
        env = {**os.environ, "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"}
        return subprocess.run(
            ["workflow-tool", *args],
            text=True,
            capture_output=True,
            encoding="utf-8",
            env=env,
            timeout=10,
        )

    def test_other_public_command(self):
        proc = self._run_cli("inspect", "draft", "+", "review")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout.strip(), "inspected")
"#
    });
    public_command_contract_result("write", &candidate, Some(authority), None).is_some_and(
        |result| {
            result.output_text.contains("[tool feedback]")
                && result
                    .output_text
                    .contains("operation_progress_class: public_command_contract_coverage_missing")
                && result.output_text.contains("candidate_target: test_tool.py")
                && result
                    .output_text
                    .contains("required_next_action: add child-process-based tests for the missing public command argv contracts")
                && result
                    .output_text
                    .contains("missing_public_commands: workflow-tool combine draft + review")
                && result
                    .metadata
                    .pointer("/tool_feedback_envelope/missing_public_commands")
                    .and_then(Value::as_array)
                    .is_some_and(|commands| {
                        commands
                            .iter()
                            .any(|command| {
                                command.as_str()
                                    == Some("workflow-tool combine draft + review")
                            })
                    })
                && alternate_command == "workflow-tool inspect draft + review"
        },
    )
}

pub fn public_command_feedback_templates_follow_target_language_fixture_passes() -> bool {
    let public_command_feedback_template_language_adapter_projection =
        "public_command_feedback_template_language_adapter_projection";
    let candidate = json!({
        "path": "workflow.test.js",
        "content": r#"
import { describe, expect, test } from "vitest";
import { spawnSync } from "node:child_process";

describe("workflow cli", () => {
  test("missing command exits with usage status", () => {
    const result = spawnSync("node", ["workflow.js", "missing", "input.txt"], {
      encoding: "utf8",
      timeout: 10000,
    });
    expect(result.status).toBe(1);
    expect(result.stdout || result.stderr).toBeTruthy();
  });
});
"#
    });
    public_command_contract_result(
        "write",
        &candidate,
        Some("CLI must treat `node workflow.js missing input.txt` as a usage error with exit code 1."),
        None,
    )
    .is_some_and(|result| {
        let templates = result
            .metadata
            .get("required_public_command_assertion_templates")
            .and_then(Value::as_array)
            .map(|templates| {
                templates
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        !templates.contains("self._run_cli")
            && !templates.contains("self.assertEqual")
            && !templates.contains("self.assertTrue")
            && result.output_text.contains(
                "required_public_command_assertion_templates:",
            )
            && public_command_feedback_template_language_adapter_projection
                == "public_command_feedback_template_language_adapter_projection"
    })
}

pub(crate) fn public_command_contract_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    let public_command_contract_fixture_workflow_neutral =
        "public_command_contract_fixture_workflow_neutral";
    public_command_contract_helper_argv_operator_fixture_passes()
        && public_command_contract_feedback_projects_typed_missing_coverage_fixture_passes()
        && public_command_contract_fixture_workflow_neutral
            == "public_command_contract_fixture_workflow_neutral"
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
    let parts = command
        .split_whitespace()
        .map(|part| part.trim_matches('"').trim_matches('\'').to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let (subject_index, explicit_command_surface) = public_command_subject_index(&parts)?;
    if !explicit_command_surface && !public_command_context_allows_command_span(context) {
        return None;
    }
    if subject_index + 1 >= parts.len() {
        return None;
    }
    let script_path = parts[subject_index].replace('\\', "/");
    let argv_after_script = parts
        .iter()
        .skip(subject_index + 1)
        .cloned()
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

fn public_command_context_allows_command_span(context: &str) -> bool {
    let lower = context.to_ascii_lowercase();
    [
        "cli",
        "public command",
        "shell command",
        "command line",
        "command-line",
        "command form",
        "command forms",
        "argv",
        "exit code",
        "stdout",
        "stderr",
        "コマンド",
        "実行",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn public_command_subject_index(parts: &[String]) -> Option<(usize, bool)> {
    let first = parts.first()?.to_ascii_lowercase();
    if matches!(first.as_str(), "python" | "python3" | "py") {
        return python_public_command_subject_index(parts).map(|index| (index, true));
    }
    if matches!(
        first.as_str(),
        "node"
            | "deno"
            | "bun"
            | "ruby"
            | "perl"
            | "php"
            | "java"
            | "go"
            | "cargo"
            | "npm"
            | "npx"
            | "pnpm"
            | "yarn"
            | "dotnet"
            | "bash"
            | "sh"
            | "pwsh"
            | "powershell"
    ) {
        return Some((public_command_runner_subject_index(parts), true));
    }
    if command_token_is_explicit_path_or_extension(&first) {
        return Some((0, true));
    }
    command_token_can_be_direct_public_command(&first).then_some((0, false))
}

fn python_public_command_subject_index(parts: &[String]) -> Option<usize> {
    let mut index = 1usize;
    while index < parts.len() {
        let token = parts[index].as_str();
        if token.ends_with(".py") {
            return Some(index);
        }
        if matches!(token, "-X" | "-W") {
            index += 2;
        } else if token == "-m" {
            let module_index = index + 1;
            let module = parts.get(module_index)?;
            if !matches!(module.as_str(), "unittest" | "py_compile" | "pytest") {
                return Some(module_index);
            }
            index += 2;
        } else if token.starts_with('-') {
            index += 1;
        } else {
            return Some(index);
        }
    }
    None
}

fn public_command_runner_subject_index(parts: &[String]) -> usize {
    let first = parts
        .first()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(first.as_str(), "cargo" | "npm" | "pnpm" | "yarn" | "dotnet") {
        return 0;
    }
    parts
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, token)| !token.starts_with('-') && !matches!(token.as_str(), "-X" | "utf8"))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn command_token_can_be_direct_public_command(token: &str) -> bool {
    command_token_is_explicit_path_or_extension(token)
        || token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn command_token_is_explicit_path_or_extension(token: &str) -> bool {
    token.starts_with("./")
        || token.starts_with(".\\")
        || token.contains('/')
        || token.contains('\\')
        || token.ends_with(".exe")
        || token.ends_with(".cmd")
        || token.ends_with(".bat")
        || token.ends_with(".sh")
}

fn public_command_subject_matches_source(subject: &str, source_name: &str) -> bool {
    let subject = normalize_path(subject);
    let source_name = normalize_path(source_name);
    subject == source_name
}

pub fn public_command_source_match_exact_target_identity_fixture_passes() -> bool {
    let public_command_source_match_exact_target_identity =
        "public_command_source_match_exact_target_identity";
    public_command_subject_matches_source("src/workflow.rs", "src/workflow.rs")
        && public_command_subject_matches_source("src\\workflow.rs", "src/workflow.rs")
        && !public_command_subject_matches_source("tools/workflow.rs", "src/workflow.rs")
        && !public_command_subject_matches_source("workflow.rs", "src/workflow.rs")
        && public_command_source_match_exact_target_identity
            == "public_command_source_match_exact_target_identity"
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
                if public_command_test_target_contract(&target).is_some() {
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
                if public_command_test_target_contract(&target).is_none() {
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
                if public_command_test_target_contract(&target).is_some() {
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

fn public_command_test_target_contract(target: &str) -> Option<PublicCommandTestTargetContract> {
    let spec = classify_language_artifact_target(target);
    if spec.role != ArtifactRole::Test
        || !matches!(spec.language, LanguageFamily::Python | LanguageFamily::Code)
    {
        return None;
    }
    Some(PublicCommandTestTargetContract {
        source_path: spec.source_path,
        language: spec.language,
    })
}

fn candidate_covers_public_command(content: &str, obligation: &PublicCommandObligation) -> bool {
    if !candidate_has_child_process_command_evidence(content)
        || !candidate_asserts_child_exit_status(content)
        || !candidate_observes_child_output(content)
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

fn candidate_has_child_process_command_evidence(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "subprocess.run",
        "spawnsync",
        "execfilesync",
        "execfile(",
        "execsync",
        "child_process",
        "command::new",
        "processbuilder",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn candidate_asserts_child_exit_status(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "returncode",
        ".status",
        "status)",
        "status.",
        "exitstatus",
        "exit_status",
        ".code",
        "success()",
        "return code",
        "exit code",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn candidate_observes_child_output(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("stdout") || lower.contains("stderr")
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
    if candidate_has_child_process_command_evidence(content)
        && !lower.contains("subprocess.run")
        && !candidate_has_child_process_timeout_authority(content)
    {
        return vec![
            "child command invocation starts a public command but no bounded timeout was found"
                .to_string(),
        ];
    }
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

fn candidate_has_child_process_timeout_authority(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    [
        "timeout:",
        "timeout=",
        ".timeout(",
        "timeout_ms:",
        "timeout_ms=",
        "wait_timeout",
        "with_timeout",
    ]
    .iter()
    .any(|needle| compact.contains(needle))
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

fn timeout_issue_feedback_sentence(target: &str, timeout_issues: &[String]) -> String {
    let issues = timeout_issues.join("; ");
    if timeout_issues_are_python_subprocess_run_specific(timeout_issues) {
        format!(
            "`{target}` starts Python `subprocess.run(...)` child commands without bounded timeout authority. Timeout issue(s): {issues}. Add a finite `timeout=` argument to every generated `subprocess.run(...)` call that can execute a child command, so verification cannot block indefinitely on an interactive or stalled child process."
        )
    } else {
        format!(
            "`{target}` starts child-process public commands without bounded timeout authority. Timeout issue(s): {issues}. Add the target language's bounded child-process timeout option, such as a spawn/command timeout or Python `subprocess.run(timeout=...)`, to every generated child command that can execute a public command, so verification cannot block indefinitely on an interactive or stalled child process."
        )
    }
}

fn timeout_issues_are_python_subprocess_run_specific(timeout_issues: &[String]) -> bool {
    !timeout_issues.is_empty()
        && timeout_issues
            .iter()
            .all(|issue| issue.to_ascii_lowercase().contains("subprocess.run"))
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
    let required_assertion_templates = missing
        .iter()
        .filter(|obligation| !obligation.output_observation_alternatives.is_empty())
        .map(|obligation| public_command_observation_assertion_template(obligation, candidate))
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
            "`{target}` does not cover prompt-visible public command contract(s): {commands}.{observations} Add child-process-based tests that execute the exact argv forms, assert return code, and assert the listed stdout/stderr observation.",
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
        issue_sentences.push(timeout_issue_feedback_sentence(
            &candidate.target,
            timeout_issues,
        ));
    }
    if !capture_issues.is_empty() {
        issue_sentences.push(format!(
            "`{target}` inspects `CompletedProcess.stdout` or `CompletedProcess.stderr` without capture authority. Capture issue(s): {issues}. Add `capture_output=True` or explicit `stdout=subprocess.PIPE` / `stderr=subprocess.PIPE` to every generated `subprocess.run(...)` call whose output streams are asserted.",
            target = candidate.target,
            issues = capture_issues.join("; ")
        ));
    }
    let typed_feedback = render_public_command_contract_feedback(
        issue_kind,
        &candidate.target,
        &missing_commands,
        &missing_observations,
        &required_assertion_templates,
        encoding_issues,
        timeout_issues,
        capture_issues,
    );
    let output_text = format!(
        "{typed_feedback}\n\nRuntime rejected `{tool_name}` before filesystem side effects because this generated test artifact violates the public command child-process contract. {} This test artifact cannot satisfy route closeout until the typed child-process coverage, encoding, timeout, and output-capture contract is coherent.",
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
            "required_public_command_assertion_templates": required_assertion_templates,
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
                "required_public_command_assertion_templates": required_assertion_templates,
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

fn render_public_command_contract_feedback(
    issue_kind: PublicCommandContractIssueKind,
    target: &str,
    missing_commands: &[String],
    missing_observations: &[String],
    required_assertion_templates: &[String],
    encoding_issues: &[String],
    timeout_issues: &[String],
    capture_issues: &[String],
) -> String {
    let mut lines = vec![
        "[tool feedback]".to_string(),
        format!(
            "operation_intent: {}",
            OperationIntent::ContentChangingAuthoringRequired.as_str()
        ),
        format!("operation_progress_class: {}", issue_kind.as_str()),
        "progress_effect: no_progress".to_string(),
        format!("candidate_target: {target}"),
        "required_next_action: add child-process-based tests for the missing public command argv contracts".to_string(),
    ];
    lines.push(format!(
        "missing_public_commands: {}",
        if missing_commands.is_empty() {
            "none".to_string()
        } else {
            missing_commands.join(", ")
        }
    ));
    if !missing_observations.is_empty() {
        lines.push(format!(
            "missing_public_command_observations: {}",
            missing_observations.join("; ")
        ));
    }
    if !required_assertion_templates.is_empty() {
        lines.push(format!(
            "required_public_command_assertion_templates: {}",
            required_assertion_templates.join(" || ")
        ));
    }
    if !encoding_issues.is_empty() {
        lines.push(format!(
            "encoding_contract_issues: {}",
            encoding_issues.join("; ")
        ));
    }
    if !timeout_issues.is_empty() {
        lines.push(format!(
            "subprocess_timeout_contract_issues: {}",
            timeout_issues.join("; ")
        ));
    }
    if !capture_issues.is_empty() {
        lines.push(format!(
            "subprocess_output_capture_contract_issues: {}",
            capture_issues.join("; ")
        ));
    }
    lines.push("side_effects_applied: false".to_string());
    lines.push(
        "submitted artifact remains rejected until the typed missing coverage and child-process execution contract are satisfied."
            .to_string(),
    );
    lines.join("\n")
}

fn public_command_observation_assertion_template(
    obligation: &PublicCommandObligation,
    candidate: &PublicCommandCandidate,
) -> String {
    let argv = obligation
        .argv_after_script
        .iter()
        .map(|arg| format!("{arg:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let alternatives = obligation
        .output_observation_alternatives
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let target_language = public_command_test_target_contract(&candidate.target)
        .map(|contract| contract.language)
        .unwrap_or(LanguageFamily::Unknown);
    if target_language == LanguageFamily::Python {
        format!(
            "proc = self._run_cli({argv}); self.assertEqual(proc.returncode, 1, f\"stdout={{proc.stdout!r}} stderr={{proc.stderr!r}}\"); self.assertTrue(any(token in (proc.stdout + proc.stderr) for token in [{alternatives}]), f\"stdout={{proc.stdout!r}} stderr={{proc.stderr!r}}\")"
        )
    } else {
        format!(
            "execute public command argv [{argv}] with the target language child-process helper; assert exit_status == 1; assert stdout/stderr contains one of [{alternatives}]"
        )
    }
}
