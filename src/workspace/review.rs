use std::process::Command;

use camino::Utf8PathBuf;

use crate::error::WorkspaceError;
use crate::session::{ReviewScope, ReviewScopeMode};

use super::{VcsKind, Workspace};

pub fn uncommitted_review_scope(workspace: &Workspace) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let status = run_git(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    let changed_files = parse_status_paths(&status);
    let staged = run_git(workspace, &["diff", "--shortstat", "--cached", "HEAD"])?;
    let unstaged = run_git(workspace, &["diff", "--shortstat"])?;
    let untracked = status
        .lines()
        .filter(|line| line.trim_start().starts_with("??"))
        .count();
    let mut summary_lines = Vec::new();
    if !staged.trim().is_empty() {
        summary_lines.push(format!("staged: {}", staged.trim()));
    }
    if !unstaged.trim().is_empty() {
        summary_lines.push(format!("unstaged: {}", unstaged.trim()));
    }
    if untracked > 0 {
        summary_lines.push(format!("untracked: {untracked} file(s)"));
    }
    if summary_lines.is_empty() {
        summary_lines.push("no uncommitted changes".to_string());
    }
    Ok(ReviewScope {
        mode: ReviewScopeMode::Uncommitted,
        base_ref: Some("HEAD".to_string()),
        head_ref: current_head_label(workspace).ok(),
        changed_files,
        summary: summary_lines.join("; "),
    })
}

pub fn branch_review_scope(
    workspace: &Workspace,
    base_ref: &str,
) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let diff_range = format!("{base_ref}...HEAD");
    let names = run_git(workspace, &["diff", "--name-only", &diff_range])?;
    let summary = run_git(workspace, &["diff", "--shortstat", &diff_range])?;
    let changed_files = names
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(Utf8PathBuf::from)
        .collect::<Vec<_>>();
    Ok(ReviewScope {
        mode: ReviewScopeMode::Branch,
        base_ref: Some(base_ref.to_string()),
        head_ref: current_head_label(workspace).ok(),
        changed_files,
        summary: if summary.trim().is_empty() {
            format!("no changes between {diff_range}")
        } else {
            summary.trim().to_string()
        },
    })
}

fn ensure_git_workspace(workspace: &Workspace) -> Result<(), WorkspaceError> {
    if workspace.vcs != VcsKind::Git {
        return Err(WorkspaceError::Message(
            "review entrypoint requires a git workspace".to_string(),
        ));
    }
    Ok(())
}

fn current_head_label(workspace: &Workspace) -> Result<String, WorkspaceError> {
    let value = run_git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(value.trim().to_string())
}

fn run_git(workspace: &Workspace, args: &[&str]) -> Result<String, WorkspaceError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace.root.as_std_path())
        .output()
        .map_err(|error| {
            WorkspaceError::Message(format!("failed to run git {}: {error}", args.join(" ")))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(WorkspaceError::Message(format!(
            "git {} failed: {}",
            args.join(" "),
            if detail.is_empty() {
                output.status.to_string()
            } else {
                detail
            }
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_status_paths(status: &str) -> Vec<Utf8PathBuf> {
    let mut paths = status
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() || line.len() < 4 {
                return None;
            }
            let path = line[3..].trim();
            if path.is_empty() {
                return None;
            }
            Some(Utf8PathBuf::from(
                path.rsplit_once(" -> ")
                    .map(|(_, after)| after)
                    .unwrap_or(path),
            ))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}
