use std::fs;

use camino::{Utf8Path, Utf8PathBuf};

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

const CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx", "kt", "php", "ps1", "py", "rb",
    "rs", "scala", "sh", "swift", "ts", "tsx",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkspaceArtifactSummary {
    file_count: usize,
    has_runtime_artifact: bool,
    only_setup_like_files: bool,
    has_rust_manifest: bool,
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
    let lower = user_text.to_ascii_lowercase();
    if summary.has_rust_manifest {
        if let Some(reason) = rust_workspace_blocked_reason(cwd, &lower) {
            return Some(reason);
        }
    }

    if summary.file_count == 0
        || summary.has_runtime_artifact
        || !summary.only_setup_like_files
        || request_is_documentation_focused(user_text)
    {
        return None;
    }

    if summary.has_rust_manifest && lower.contains("rust") {
        return Some(
            "completion blocked: the workspace still only contains Rust setup files. After creating `Cargo.toml`, also create `src/main.rs` or `src/lib.rs` before finishing.".to_string(),
        );
    }

    if requires_workspace_changes(user_text) {
        return Some(
            "completion blocked: the workspace still only contains setup or documentation files. Create the requested implementation files before finishing.".to_string(),
        );
    }

    None
}

fn rust_workspace_blocked_reason(cwd: &Utf8Path, lower_user_text: &str) -> Option<String> {
    if !lower_user_text.contains("rust") || lower_user_text.contains("hello world") {
        return None;
    }

    for project_root in candidate_rust_project_roots(cwd) {
        if rust_project_lacks_source(&project_root) {
            if project_root == cwd {
                return Some(
                    "completion blocked: the workspace still only contains Rust setup files. After creating `Cargo.toml`, also create `src/main.rs` or `src/lib.rs` before finishing.".to_string(),
                );
            }

            let manifest_path = workspace_relative_hint(cwd, &project_root.join("Cargo.toml"));
            let main_path = workspace_relative_hint(cwd, &project_root.join("src/main.rs"));
            let lib_path = workspace_relative_hint(cwd, &project_root.join("src/lib.rs"));
            let project_label = workspace_relative_hint(cwd, &project_root);
            return Some(format!(
                "completion blocked: the Rust project at `{project_label}/` still only contains setup files. After creating `{manifest_path}`, also create `{main_path}` or `{lib_path}` before finishing."
            ));
        }

        if rust_project_looks_like_cargo_init_scaffold(&project_root) {
            if project_root == cwd {
                return Some(
                    "completion blocked: the workspace is still the default `cargo init` scaffold. Replace the `Hello, world!` stub with the requested Rust implementation before finishing.".to_string(),
                );
            }

            let project_label = workspace_relative_hint(cwd, &project_root);
            let main_path = workspace_relative_hint(cwd, &project_root.join("src/main.rs"));
            return Some(format!(
                "completion blocked: the Rust project at `{project_label}/` is still the default `cargo init` scaffold. Replace the `Hello, world!` stub in `{main_path}` with the requested Rust implementation before finishing."
            ));
        }
    }

    None
}

fn rust_project_looks_like_cargo_init_scaffold(project_root: &Utf8Path) -> bool {
    let main_rs = project_root.join("src/main.rs");
    let main_text = fs::read_to_string(&main_rs)
        .ok()
        .map(|value| value.replace("\r\n", "\n"))
        .unwrap_or_default();
    if main_text.trim() != "fn main() {\n    println!(\"Hello, world!\");\n}" {
        return false;
    }

    rust_project_source_files(project_root) == vec!["src/main.rs".to_string()]
}

fn rust_project_lacks_source(project_root: &Utf8Path) -> bool {
    rust_project_source_files(project_root).is_empty()
}

fn rust_project_source_files(project_root: &Utf8Path) -> Vec<String> {
    let mut files = Vec::new();
    for relative_dir in ["src", "tests", "examples", "benches"] {
        collect_rust_source_files(project_root, &project_root.join(relative_dir), &mut files);
    }

    let build_rs = project_root.join("build.rs");
    if build_rs.is_file() {
        files.push("build.rs".to_string());
    }

    files.sort();
    files.dedup();
    files
}

fn collect_rust_source_files(project_root: &Utf8Path, current: &Utf8Path, files: &mut Vec<String>) {
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
            if should_skip_directory(project_root, &path) {
                continue;
            }
            collect_rust_source_files(project_root, &path, files);
            continue;
        }
        if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        {
            files.push(relative_lowercase(project_root, &path));
        }
    }
}

fn candidate_rust_project_roots(cwd: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut roots = Vec::new();
    if cwd.join("Cargo.toml").is_file() {
        roots.push(cwd.to_path_buf());
    }

    let Ok(entries) = fs::read_dir(cwd) else {
        return roots;
    };
    for entry in entries.flatten() {
        let path = match Utf8PathBuf::from_path_buf(entry.path()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || should_skip_directory(cwd, &path) {
            continue;
        }
        if path.join("Cargo.toml").is_file() {
            roots.push(path);
        }
    }

    roots.sort();
    roots.dedup();
    roots
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
        !candidate.is_empty()
            && (candidate.contains('/') || candidate.contains('\\') || candidate.contains('.'))
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
            summary.only_setup_like_files = false;
        } else if !is_setup_like_artifact(root, &path) {
            summary.only_setup_like_files = false;
        }

        let relative = relative_lowercase(root, &path);
        if relative == "cargo.toml" || relative.ends_with("/cargo.toml") {
            summary.has_rust_manifest = true;
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
    if relative.starts_with("src/") || relative.starts_with("tests/") {
        return true;
    }
    let filename = relative.rsplit('/').next().unwrap_or(relative.as_str());
    if filename.starts_with("test_") {
        return true;
    }
    path.extension()
        .map(|extension| CODE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
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
