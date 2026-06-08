use std::fs;

use camino::{Utf8Path, Utf8PathBuf};

use crate::agent::language_evidence::{
    ArtifactRole, classify_artifact_target as classify_language_artifact_target,
};

// These setup-like files are intentionally hardcoded because completion gating
// must distinguish "project scaffold exists" from "runtime artifact exists".
// opencode and Roo Code also hardcode runtime guardrails and prompt budgets;
// moyai keeps this list explicit so local LLMs cannot claim completion
// after creating only manifests, lockfiles, or documentation.
const SETUP_LIKE_FILENAMES: &[&str] = &[
    ".cargo/config.toml",
    ".editorconfig",
    ".gitignore",
    "cargo.lock",
    "cargo.toml",
    "composer.json",
    "config.toml",
    "go.mod",
    "go.sum",
    "gemfile",
    "instruction.md",
    "instructions.md",
    "package-lock.json",
    "package.json",
    "pipfile",
    "pipfile.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "pyproject.toml",
    "readme",
    "readme.md",
    "requirements.txt",
    "task.md",
    "task.txt",
    "tsconfig.json",
    "yarn.lock",
];

const DOCUMENTATION_HINTS: &[&str] = &[
    "comment",
    "comments",
    "design",
    "doc",
    "docs",
    "documentation",
    "markdown",
    "readme",
    "spec",
];

const CREATION_HINTS: &[&str] = &[
    "create",
    "implement",
    "build",
    "make",
    "write",
    "edit",
    "update",
    "modify",
    "fix",
    "refactor",
    "add ",
    "作成",
    "実装",
    "更新",
    "修正",
    "変更",
    "追加",
    "書",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkspaceArtifactSummary {
    file_count: usize,
    has_runtime_artifact: bool,
    only_setup_like_files: bool,
    runtime_artifact_count: usize,
    default_scaffold_runtime_artifacts: Vec<String>,
}

pub(crate) fn requires_workspace_changes(user_text: &str) -> bool {
    let lower = user_text.to_ascii_lowercase();
    CREATION_HINTS.iter().any(|needle| lower.contains(needle))
}

pub(crate) fn completion_workspace_blocked_reason(
    cwd: &Utf8Path,
    latest_user_text: Option<&str>,
) -> Option<String> {
    let user_text = latest_user_text?.trim();
    if user_text.is_empty() || contains_explicit_file_target(user_text) {
        return None;
    }

    let summary = inspect_workspace(cwd);
    let request_requires_workspace_change = requires_workspace_changes(user_text);
    if summary.has_runtime_artifact
        && summary.runtime_artifact_count == summary.default_scaffold_runtime_artifacts.len()
        && request_requires_workspace_change
        && !user_text.to_ascii_lowercase().contains("hello world")
        && !user_text.to_ascii_lowercase().contains("hello, world")
    {
        let targets = summary.default_scaffold_runtime_artifacts.join(", ");
        return Some(format!(
            "completion blocked: runtime artifacts still look like default scaffold stubs (`{targets}`). Replace generated placeholder code with the requested implementation before finishing."
        ));
    }

    if summary.file_count == 0
        || summary.has_runtime_artifact
        || !summary.only_setup_like_files
        || request_is_documentation_focused(user_text)
    {
        return None;
    }

    if request_requires_workspace_change {
        return Some(
            "completion blocked: the workspace still only contains setup or documentation files. Create the requested implementation files before finishing.".to_string(),
        );
    }

    None
}

fn request_is_documentation_focused(user_text: &str) -> bool {
    let lower = user_text.to_ascii_lowercase();
    DOCUMENTATION_HINTS
        .iter()
        .any(|needle| lower.contains(needle))
        && !requires_workspace_changes(user_text)
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
        if candidate.is_empty() {
            return false;
        }
        if candidate.contains('/') || candidate.contains('\\') {
            return true;
        }
        let lower = candidate.to_ascii_lowercase();
        SETUP_LIKE_FILENAMES.contains(&lower.as_str())
    })
}

fn inspect_workspace(cwd: &Utf8Path) -> WorkspaceArtifactSummary {
    let mut summary = WorkspaceArtifactSummary {
        only_setup_like_files: true,
        ..WorkspaceArtifactSummary::default()
    };
    collect_workspace_artifacts(cwd, cwd, &mut summary);
    summary
}

fn collect_workspace_artifacts(
    root: &Utf8Path,
    current: &Utf8Path,
    summary: &mut WorkspaceArtifactSummary,
) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };

    for entry in entries.flatten() {
        let path = match Utf8PathBuf::from_path_buf(entry.path()) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if should_skip_directory(root, &path) {
                continue;
            }
            collect_workspace_artifacts(root, &path, summary);
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        summary.file_count += 1;
        if is_runtime_artifact(root, &path) {
            summary.has_runtime_artifact = true;
            summary.runtime_artifact_count += 1;
            summary.only_setup_like_files = false;
            if runtime_artifact_looks_like_default_scaffold_stub(root, &path) {
                summary
                    .default_scaffold_runtime_artifacts
                    .push(workspace_relative_hint(root, &path));
            }
        } else if !is_setup_like_artifact(root, &path) {
            summary.only_setup_like_files = false;
        }
    }
}

fn should_skip_directory(root: &Utf8Path, path: &Utf8Path) -> bool {
    relative_lowercase(root, path).split('/').any(|component| {
        matches!(
            component,
            ".git" | "target" | "node_modules" | "__pycache__"
        )
    })
}

fn is_runtime_artifact(root: &Utf8Path, path: &Utf8Path) -> bool {
    let relative = relative_lowercase(root, path);
    if is_setup_like_artifact(root, path) {
        return false;
    }
    if relative.starts_with("src/") || relative.starts_with("tests/") {
        return true;
    }
    let spec = classify_language_artifact_target(&relative);
    matches!(spec.role, ArtifactRole::Source | ArtifactRole::Test)
}

fn runtime_artifact_looks_like_default_scaffold_stub(root: &Utf8Path, path: &Utf8Path) -> bool {
    let relative = relative_lowercase(root, path);
    let spec = classify_language_artifact_target(&relative);
    if !matches!(spec.role, ArtifactRole::Source | ArtifactRole::Test) {
        return false;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let normalized = text.replace("\r\n", "\n").to_ascii_lowercase();
    let non_empty_line_count = normalized
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_small_hello_world_stub = non_empty_line_count <= 12
        && (compact.contains("hello, world") || compact.contains("hello world"));
    let has_frontend_generator_stub = compact.contains("vite")
        && (compact.contains("count is") || compact.contains("edit src/"))
        || compact.contains("learn react")
        || compact.contains("reactlogo")
        || compact.contains("app.css") && compact.contains("main.tsx");
    has_small_hello_world_stub || has_frontend_generator_stub
}

fn is_setup_like_artifact(root: &Utf8Path, path: &Utf8Path) -> bool {
    let relative = relative_lowercase(root, path);
    if SETUP_LIKE_FILENAMES.contains(&relative.as_str()) {
        return true;
    }

    let filename = relative.rsplit('/').next().unwrap_or(relative.as_str());
    if matches!(filename, "readme" | "readme.md") || relative.starts_with("docs/") {
        return true;
    }

    matches!(
        path.extension().map(|value| value.to_ascii_lowercase()),
        Some(ref extension)
            if matches!(extension.as_str(), "adoc" | "json" | "md" | "rst" | "toml" | "txt" | "yaml" | "yml")
    )
}

fn relative_lowercase(root: &Utf8Path, path: &Utf8Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .as_str()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

fn workspace_relative_hint(root: &Utf8Path, path: &Utf8Path) -> String {
    let relative = relative_lowercase(root, path);
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    }
}

pub(crate) fn generic_scaffold_completion_guard_fixture_passes() -> bool {
    let root_path = std::env::temp_dir().join(format!(
        "moyai-generic-completion-guard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let setup_only = root.join("setup-only");
    let default_stub = root.join("default-stub");
    let implemented = root.join("implemented");
    let result = (|| {
        fs::create_dir_all(setup_only.as_std_path()).ok()?;
        fs::write(
            setup_only.join("package.json").as_std_path(),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .ok()?;
        let setup_reason = completion_workspace_blocked_reason(
            setup_only.as_path(),
            Some("Create a todo CLI application"),
        )?;

        fs::create_dir_all(default_stub.join("src").as_std_path()).ok()?;
        fs::write(
            default_stub.join("package.json").as_std_path(),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .ok()?;
        fs::write(
            default_stub.join("src/main.js").as_std_path(),
            "console.log(\"Hello, world!\");\n",
        )
        .ok()?;
        let stub_reason = completion_workspace_blocked_reason(
            default_stub.as_path(),
            Some("Create a todo CLI application"),
        )?;

        fs::create_dir_all(implemented.join("src").as_std_path()).ok()?;
        fs::write(
            implemented.join("package.json").as_std_path(),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .ok()?;
        fs::write(
            implemented.join("src/main.js").as_std_path(),
            "export function addTodo(items, title) {\n  return [...items, { title, done: false }];\n}\n",
        )
        .ok()?;
        let implemented_reason = completion_workspace_blocked_reason(
            implemented.as_path(),
            Some("Create a todo CLI application"),
        );
        Some(
            setup_reason.contains("setup or documentation files")
                && stub_reason.contains("default scaffold stubs")
                && stub_reason.contains("src/main.js")
                && implemented_reason.is_none(),
        )
    })()
    .unwrap_or(false);
    let _ = fs::remove_dir_all(root.as_std_path());
    result
}

pub(crate) fn completion_guard_does_not_treat_dotted_technology_token_as_file_target_fixture_passes()
-> bool {
    let root_path = std::env::temp_dir().join(format!(
        "moyai-dotted-technology-completion-guard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let Ok(root) = Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let result = (|| {
        fs::create_dir_all(root.as_std_path()).ok()?;
        fs::write(
            root.join("package.json").as_std_path(),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .ok()?;

        let dotted_technology_reason = completion_workspace_blocked_reason(
            root.as_path(),
            Some("Create a Node.js todo CLI application"),
        )?;
        let explicit_file_reason =
            completion_workspace_blocked_reason(root.as_path(), Some("Create src/main.js"));

        Some(
            dotted_technology_reason.contains("setup or documentation files")
                && explicit_file_reason.is_none(),
        )
    })()
    .unwrap_or(false);
    let _ = fs::remove_dir_all(root.as_std_path());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_guard_does_not_treat_dotted_technology_token_as_file_target() {
        assert!(
            completion_guard_does_not_treat_dotted_technology_token_as_file_target_fixture_passes()
        );
    }
}
