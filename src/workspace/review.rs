use std::process::Command;

use camino::Utf8PathBuf;

use crate::error::WorkspaceError;
use crate::session::{ReviewScope, ReviewScopeMode};

use super::{VcsKind, Workspace};

pub fn uncommitted_review_scope(workspace: &Workspace) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let status = run_git_bytes(
        workspace,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let status_entries = parse_status_entries(&status)?;
    let changed_files = status_entries
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    let staged = run_git(workspace, &["diff", "--shortstat", "--cached", "HEAD"])?;
    let unstaged = run_git(workspace, &["diff", "--shortstat"])?;
    let untracked = status_entries
        .iter()
        .filter(|(code, _)| code == "??")
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
    run_git(
        workspace,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base_ref}^{{commit}}"),
        ],
    )?;
    let diff_range = format!("{base_ref}...HEAD");
    let names = run_git_bytes(
        workspace,
        &["diff", "--name-only", "-z", "--end-of-options", &diff_range],
    )?;
    let summary = run_git(
        workspace,
        &["diff", "--shortstat", "--end-of-options", &diff_range],
    )?;
    let changed_files = parse_nul_paths(&names)?;
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
    let output = run_git_bytes(workspace, args)?;
    String::from_utf8(output).map_err(|error| {
        WorkspaceError::Message(format!(
            "git {} returned non-UTF-8 output: {error}",
            args.join(" ")
        ))
    })
}

fn run_git_bytes(workspace: &Workspace, args: &[&str]) -> Result<Vec<u8>, WorkspaceError> {
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
    Ok(output.stdout)
}

fn parse_status_entries(status: &[u8]) -> Result<Vec<(String, Utf8PathBuf)>, WorkspaceError> {
    let records = status.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.is_empty() {
            index += 1;
            continue;
        }
        if record.len() < 4 || record[2] != b' ' {
            return Err(WorkspaceError::Message(
                "git status returned an invalid porcelain record".to_string(),
            ));
        }
        let code = std::str::from_utf8(&record[..2])
            .map_err(|error| WorkspaceError::Message(format!("invalid git status code: {error}")))?
            .to_string();
        let path = std::str::from_utf8(&record[3..]).map_err(|error| {
            WorkspaceError::Message(format!("git status path is not valid UTF-8: {error}"))
        })?;
        if !path.is_empty() {
            entries.push((code.clone(), Utf8PathBuf::from(path)));
        }
        index += 1;
        if code
            .as_bytes()
            .iter()
            .any(|code| matches!(*code, b'R' | b'C'))
        {
            index += 1;
        }
    }
    entries.sort_by(|left, right| left.1.cmp(&right.1));
    entries.dedup_by(|left, right| left.1 == right.1);
    Ok(entries)
}

fn parse_nul_paths(bytes: &[u8]) -> Result<Vec<Utf8PathBuf>, WorkspaceError> {
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            std::str::from_utf8(record)
                .map(Utf8PathBuf::from)
                .map_err(|error| {
                    WorkspaceError::Message(format!("git path is not valid UTF-8: {error}"))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_z_parser_preserves_utf8_and_rename_destination() {
        let input = " M src/日本語.rs\0R  src/new name.rs\0src/old name.rs\0?? odd -> name.txt\0";

        let entries = parse_status_entries(input.as_bytes()).expect("status entries");

        assert_eq!(
            entries
                .iter()
                .map(|(_, path)| path.as_str())
                .collect::<Vec<_>>(),
            vec!["odd -> name.txt", "src/new name.rs", "src/日本語.rs"]
        );
    }
}
