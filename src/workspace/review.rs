use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;

use super::{VcsKind, Workspace};

const GIT_REVIEW_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_REVIEW_STDOUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const GIT_REVIEW_STDERR_LIMIT_BYTES: usize = 256 * 1024;
const GIT_REVIEW_PATH_LIMIT: usize = 50_000;
const GIT_REVIEW_WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewScopeMode {
    Uncommitted,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    pub mode: ReviewScopeMode,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    #[serde(default)]
    pub changed_files: Vec<Utf8PathBuf>,
    pub summary: String,
}

impl ReviewScope {
    pub fn label(&self) -> String {
        match self.mode {
            ReviewScopeMode::Uncommitted => "review_uncommitted".to_string(),
            ReviewScopeMode::Branch => match (&self.base_ref, &self.head_ref) {
                (Some(base), Some(head)) => format!("review_branch:{base}...{head}"),
                (Some(base), None) => format!("review_branch:{base}"),
                _ => "review_branch".to_string(),
            },
        }
    }
}

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
    let mut child = Command::new("git")
        .args(args)
        .current_dir(workspace.root.as_std_path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            WorkspaceError::Message(format!("failed to run git {}: {error}", args.join(" ")))
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        WorkspaceError::Message("failed to capture git review stdout".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        WorkspaceError::Message("failed to capture git review stderr".to_string())
    })?;
    let stdout_reader =
        thread::spawn(move || read_bounded_stream(stdout, GIT_REVIEW_STDOUT_LIMIT_BYTES));
    let stderr_reader =
        thread::spawn(move || read_bounded_stream(stderr, GIT_REVIEW_STDERR_LIMIT_BYTES));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < GIT_REVIEW_OPERATION_TIMEOUT => {
                thread::sleep(GIT_REVIEW_WAIT_POLL);
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_bounded_reader(stdout_reader, "stdout");
                let _ = join_bounded_reader(stderr_reader, "stderr");
                return Err(WorkspaceError::Message(format!(
                    "git {} exceeded the {} second review-operation deadline",
                    args.join(" "),
                    GIT_REVIEW_OPERATION_TIMEOUT.as_secs()
                )));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_bounded_reader(stdout_reader, "stdout");
                let _ = join_bounded_reader(stderr_reader, "stderr");
                return Err(WorkspaceError::Message(format!(
                    "failed while waiting for git {}: {error}",
                    args.join(" ")
                )));
            }
        }
    };
    let stdout = join_bounded_reader(stdout_reader, "stdout")?;
    let stderr = join_bounded_reader(stderr_reader, "stderr")?;
    if stdout.overflowed {
        return Err(WorkspaceError::Message(format!(
            "git {} exceeded the bounded review stdout limit of {} bytes",
            args.join(" "),
            GIT_REVIEW_STDOUT_LIMIT_BYTES
        )));
    }
    if stderr.overflowed {
        return Err(WorkspaceError::Message(format!(
            "git {} exceeded the bounded review stderr limit of {} bytes",
            args.join(" "),
            GIT_REVIEW_STDERR_LIMIT_BYTES
        )));
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr.bytes).trim().to_string();
        let stdout = String::from_utf8_lossy(&stdout.bytes).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(WorkspaceError::Message(format!(
            "git {} failed: {}",
            args.join(" "),
            if detail.is_empty() {
                status.to_string()
            } else {
                detail
            }
        )));
    }
    Ok(stdout.bytes)
}

struct BoundedStreamCapture {
    bytes: Vec<u8>,
    overflowed: bool,
}

fn read_bounded_stream(
    mut stream: impl Read,
    limit: usize,
) -> std::io::Result<BoundedStreamCapture> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut overflowed = false;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let retained = limit.saturating_sub(bytes.len()).min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        overflowed |= retained < read;
    }
    Ok(BoundedStreamCapture { bytes, overflowed })
}

fn join_bounded_reader(
    reader: thread::JoinHandle<std::io::Result<BoundedStreamCapture>>,
    stream_name: &str,
) -> Result<BoundedStreamCapture, WorkspaceError> {
    reader
        .join()
        .map_err(|_| WorkspaceError::Message(format!("git review {stream_name} reader panicked")))?
        .map_err(|error| {
            WorkspaceError::Message(format!(
                "failed to read bounded git review {stream_name}: {error}"
            ))
        })
}

fn parse_status_entries(status: &[u8]) -> Result<Vec<(String, Utf8PathBuf)>, WorkspaceError> {
    parse_status_entries_with_limit(status, GIT_REVIEW_PATH_LIMIT)
}

fn parse_status_entries_with_limit(
    status: &[u8],
    path_limit: usize,
) -> Result<Vec<(String, Utf8PathBuf)>, WorkspaceError> {
    let mut entries = Vec::new();
    let mut records = status.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
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
            if entries.len() >= path_limit {
                return Err(WorkspaceError::Message(format!(
                    "git review changed-path count exceeded the limit of {path_limit}"
                )));
            }
            entries.push((code.clone(), Utf8PathBuf::from(path)));
        }
        if code
            .as_bytes()
            .iter()
            .any(|code| matches!(*code, b'R' | b'C'))
        {
            records.next().ok_or_else(|| {
                WorkspaceError::Message(
                    "git status rename record is missing its source path".to_string(),
                )
            })?;
        }
    }
    entries.sort_by(|left, right| left.1.cmp(&right.1));
    entries.dedup_by(|left, right| left.1 == right.1);
    Ok(entries)
}

fn parse_nul_paths(bytes: &[u8]) -> Result<Vec<Utf8PathBuf>, WorkspaceError> {
    parse_nul_paths_with_limit(bytes, GIT_REVIEW_PATH_LIMIT)
}

fn parse_nul_paths_with_limit(
    bytes: &[u8],
    path_limit: usize,
) -> Result<Vec<Utf8PathBuf>, WorkspaceError> {
    let mut paths = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if paths.len() >= path_limit {
            return Err(WorkspaceError::Message(format!(
                "git review changed-path count exceeded the limit of {path_limit}"
            )));
        }
        paths.push(
            std::str::from_utf8(record)
                .map(Utf8PathBuf::from)
                .map_err(|error| {
                    WorkspaceError::Message(format!("git path is not valid UTF-8: {error}"))
                })?,
        );
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

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

    #[test]
    fn bounded_git_capture_drains_but_retains_only_the_limit() {
        let capture =
            read_bounded_stream(Cursor::new(vec![b'x'; 64]), 12).expect("bounded stream capture");

        assert_eq!(capture.bytes, vec![b'x'; 12]);
        assert!(capture.overflowed);
    }

    #[test]
    fn changed_path_parsers_fail_instead_of_returning_partial_scope() {
        let status = b" M a.txt\0 M b.txt\0 M c.txt\0";
        let status_error = parse_status_entries_with_limit(status, 2)
            .expect_err("status path limit must fail closed");
        assert!(status_error.to_string().contains("exceeded"));

        let names = b"a.txt\0b.txt\0c.txt\0";
        let names_error =
            parse_nul_paths_with_limit(names, 2).expect_err("name path limit must fail closed");
        assert!(names_error.to_string().contains("exceeded"));
    }
}
