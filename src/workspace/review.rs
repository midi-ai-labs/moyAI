use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::WorkspaceError;

use super::PathGuard;
use super::path_guard::ExistingObjectIdentity;
use super::{VcsKind, Workspace};

const GIT_REVIEW_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_REVIEW_STDOUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const GIT_REVIEW_STDERR_LIMIT_BYTES: usize = 256 * 1024;
const GIT_REVIEW_PATH_LIMIT: usize = 50_000;
const GIT_REVIEW_WAIT_POLL: Duration = Duration::from_millis(10);
const GIT_REVIEW_UNTRACKED_BYTES_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct ReviewDeadline {
    end: Instant,
    scope: Duration,
}

impl ReviewDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            end: Instant::now() + timeout,
            scope: timeout,
        }
    }

    fn remaining(&self, operation: &str) -> Result<Duration, WorkspaceError> {
        let remaining = self.end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(WorkspaceError::Message(format!(
                "git review exceeded its {} second scope deadline while {operation}",
                self.scope.as_secs_f64()
            )));
        }
        Ok(remaining)
    }

    fn ensure_remaining(&self, operation: &str) -> Result<(), WorkspaceError> {
        self.remaining(operation).map(|_| ())
    }
}

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
    uncommitted_review_scope_with_observer(workspace, || {})
}

fn uncommitted_review_scope_with_observer(
    workspace: &Workspace,
    observer: impl FnOnce(),
) -> Result<ReviewScope, WorkspaceError> {
    uncommitted_review_scope_with_deadline_and_observer(
        workspace,
        GIT_REVIEW_OPERATION_TIMEOUT,
        observer,
    )
}

fn uncommitted_review_scope_with_deadline_and_observer(
    workspace: &Workspace,
    timeout: Duration,
    observer: impl FnOnce(),
) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let deadline = ReviewDeadline::new(timeout);
    let first = capture_uncommitted_snapshot(workspace, &deadline, || {})?;
    observer();
    let second = capture_uncommitted_snapshot(workspace, &deadline, || {})?;
    finish_uncommitted_scope(first, second, &deadline)
}

#[cfg(test)]
fn uncommitted_review_scope_with_capture_observer(
    workspace: &Workspace,
    observer: impl FnOnce(),
) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let deadline = ReviewDeadline::new(GIT_REVIEW_OPERATION_TIMEOUT);
    let first = capture_uncommitted_snapshot(workspace, &deadline, observer)?;
    let second = capture_uncommitted_snapshot(workspace, &deadline, || {})?;
    finish_uncommitted_scope(first, second, &deadline)
}

fn finish_uncommitted_scope(
    first: UncommittedReviewSnapshot,
    second: UncommittedReviewSnapshot,
    deadline: &ReviewDeadline,
) -> Result<ReviewScope, WorkspaceError> {
    deadline.ensure_remaining("finalizing an uncommitted review inventory")?;
    if first != second {
        return Err(WorkspaceError::Message(
            "git uncommitted review state changed while fixing its inventory; retry the review"
                .to_string(),
        ));
    }
    let status_entries = parse_status_entries(&second.status)?;
    let changed_files = status_entries
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    let untracked = status_entries
        .iter()
        .filter(|(code, _)| code == "??")
        .count();
    let mut summary_lines = Vec::new();
    if !second.staged.trim().is_empty() {
        summary_lines.push(format!("staged: {}", second.staged.trim()));
    }
    if !second.unstaged.trim().is_empty() {
        summary_lines.push(format!("unstaged: {}", second.unstaged.trim()));
    }
    if untracked > 0 {
        summary_lines.push(format!("untracked: {untracked} file(s)"));
    }
    if summary_lines.is_empty() {
        summary_lines.push("no uncommitted changes".to_string());
    }
    deadline.ensure_remaining("returning an uncommitted review inventory")?;
    Ok(ReviewScope {
        mode: ReviewScopeMode::Uncommitted,
        base_ref: Some("HEAD".to_string()),
        head_ref: Some(second.head.label().to_string()),
        changed_files,
        summary: summary_lines.join("; "),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitHeadState {
    Commit { oid: String, label: String },
    Unborn { symbolic_ref: String },
}

impl GitHeadState {
    fn label(&self) -> &str {
        match self {
            Self::Commit { label, .. } => label,
            Self::Unborn { symbolic_ref } => symbolic_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(symbolic_ref),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitOutputFingerprint {
    byte_len: usize,
    sha256: [u8; 32],
}

impl GitOutputFingerprint {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            byte_len: bytes.len(),
            sha256: Sha256::digest(bytes).into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UntrackedEntryKind {
    RegularFile,
    SymbolicLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UntrackedEntryFingerprint {
    path: Utf8PathBuf,
    kind: UntrackedEntryKind,
    identity: UntrackedEntryIdentity,
    byte_len: u64,
    sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UntrackedEntryIdentity {
    Opened(ExistingObjectIdentity),
}

#[derive(Debug, PartialEq, Eq)]
struct UncommittedReviewSnapshot {
    head: GitHeadState,
    status: Vec<u8>,
    content: UncommittedContentFingerprint,
    staged: String,
    unstaged: String,
}

#[derive(Debug, PartialEq, Eq)]
struct UncommittedContentFingerprint {
    staged: GitOutputFingerprint,
    unstaged: GitOutputFingerprint,
    untracked: Vec<UntrackedEntryFingerprint>,
}

fn capture_uncommitted_snapshot(
    workspace: &Workspace,
    deadline: &ReviewDeadline,
    observer: impl FnOnce(),
) -> Result<UncommittedReviewSnapshot, WorkspaceError> {
    deadline.ensure_remaining("starting an uncommitted inventory capture")?;
    let head = resolve_head_state(workspace, deadline)?;
    let status = capture_status(workspace, deadline)?;
    let status_entries = parse_status_entries(&status)?;
    let content = capture_content_fingerprint(workspace, &head, &status_entries, deadline)?;

    observer();

    let staged = capture_diff_summary(workspace, &head, true, deadline)?;
    let unstaged = capture_diff_summary(workspace, &head, false, deadline)?;
    let final_status = capture_status(workspace, deadline)?;
    let final_head = resolve_head_state(workspace, deadline)?;
    let final_status_entries = parse_status_entries(&final_status)?;
    let final_content =
        capture_content_fingerprint(workspace, &final_head, &final_status_entries, deadline)?;
    let tail_status = capture_status(workspace, deadline)?;
    let tail_head = resolve_head_state(workspace, deadline)?;
    if status != final_status
        || final_status != tail_status
        || head != final_head
        || final_head != tail_head
        || content != final_content
    {
        return Err(WorkspaceError::Message(
            "git uncommitted review state changed during an inventory capture; retry the review"
                .to_string(),
        ));
    }

    Ok(UncommittedReviewSnapshot {
        head,
        status,
        content,
        staged,
        unstaged,
    })
}

fn capture_content_fingerprint(
    workspace: &Workspace,
    head: &GitHeadState,
    status_entries: &[(String, Utf8PathBuf)],
    deadline: &ReviewDeadline,
) -> Result<UncommittedContentFingerprint, WorkspaceError> {
    Ok(UncommittedContentFingerprint {
        staged: capture_diff_fingerprint(workspace, head, true, deadline)?,
        unstaged: capture_diff_fingerprint(workspace, head, false, deadline)?,
        untracked: capture_untracked_fingerprints(workspace, status_entries, deadline)?,
    })
}

pub fn branch_review_scope(
    workspace: &Workspace,
    base_ref: &str,
) -> Result<ReviewScope, WorkspaceError> {
    branch_review_scope_with_observer(workspace, base_ref, || {})
}

fn branch_review_scope_with_observer(
    workspace: &Workspace,
    base_ref: &str,
    observer: impl FnOnce(),
) -> Result<ReviewScope, WorkspaceError> {
    ensure_git_workspace(workspace)?;
    let deadline = ReviewDeadline::new(GIT_REVIEW_OPERATION_TIMEOUT);
    let base_commit = resolve_commit(workspace, base_ref, &deadline)?;
    let head_commit = resolve_commit(workspace, "HEAD", &deadline)?;
    let head_label = current_head_label(workspace, &deadline)?;
    let diff_range = format!("{base_commit}...{head_commit}");
    let names = run_git_bytes(
        workspace,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--name-only",
            "-z",
            "--end-of-options",
            &diff_range,
        ],
        &deadline,
    )?;
    observer();
    let summary = run_git(
        workspace,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--shortstat",
            "--end-of-options",
            &diff_range,
        ],
        &deadline,
    )?;
    if resolve_commit(workspace, base_ref, &deadline)? != base_commit
        || resolve_commit(workspace, "HEAD", &deadline)? != head_commit
        || current_head_label(workspace, &deadline)? != head_label
    {
        return Err(WorkspaceError::Message(
            "git branch review refs changed while fixing their inventory; retry the review"
                .to_string(),
        ));
    }
    deadline.ensure_remaining("finalizing a branch review inventory")?;
    let changed_files = parse_nul_paths(&names)?;
    deadline.ensure_remaining("returning a branch review inventory")?;
    Ok(ReviewScope {
        mode: ReviewScopeMode::Branch,
        base_ref: Some(base_ref.to_string()),
        head_ref: Some(head_label),
        changed_files,
        summary: if summary.trim().is_empty() {
            format!("no changes between {base_ref}...HEAD")
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

fn current_head_label(
    workspace: &Workspace,
    deadline: &ReviewDeadline,
) -> Result<String, WorkspaceError> {
    let value = run_git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"], deadline)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(WorkspaceError::Message(
            "git HEAD resolved to an empty display label".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn resolve_commit(
    workspace: &Workspace,
    reference: &str,
    deadline: &ReviewDeadline,
) -> Result<String, WorkspaceError> {
    let value = run_git(
        workspace,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
        deadline,
    )?;
    let value = value.trim();
    if value.is_empty() {
        return Err(WorkspaceError::Message(format!(
            "git reference `{reference}` resolved to an empty commit identity"
        )));
    }
    Ok(value.to_string())
}

fn resolve_head_state(
    workspace: &Workspace,
    deadline: &ReviewDeadline,
) -> Result<GitHeadState, WorkspaceError> {
    match resolve_commit(workspace, "HEAD", deadline) {
        Ok(oid) => Ok(GitHeadState::Commit {
            oid,
            label: current_head_label(workspace, deadline)?,
        }),
        Err(commit_error) => {
            let symbolic_ref =
                match run_git(workspace, &["symbolic-ref", "--quiet", "HEAD"], deadline) {
                    Ok(symbolic_ref) => symbolic_ref,
                    Err(_) => {
                        deadline.ensure_remaining("classifying git HEAD")?;
                        return Err(commit_error);
                    }
                };
            let symbolic_ref = symbolic_ref.trim();
            if symbolic_ref.is_empty() {
                return Err(WorkspaceError::Message(
                    "unborn git HEAD resolved to an empty symbolic reference".to_string(),
                ));
            }
            if exact_git_reference_exists(workspace, symbolic_ref, deadline)? {
                return Err(commit_error);
            }
            Ok(GitHeadState::Unborn {
                symbolic_ref: symbolic_ref.to_string(),
            })
        }
    }
}

fn exact_git_reference_exists(
    workspace: &Workspace,
    reference: &str,
    deadline: &ReviewDeadline,
) -> Result<bool, WorkspaceError> {
    let args = ["show-ref", "--verify", "--quiet", reference];
    let output = capture_git_command(workspace, &args, deadline)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_command_failure(&args, &output)),
    }
}

fn capture_status(
    workspace: &Workspace,
    deadline: &ReviewDeadline,
) -> Result<Vec<u8>, WorkspaceError> {
    run_git_bytes(
        workspace,
        &[
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "submodule.recurse=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ],
        deadline,
    )
}

fn capture_diff_fingerprint(
    workspace: &Workspace,
    head: &GitHeadState,
    staged: bool,
    deadline: &ReviewDeadline,
) -> Result<GitOutputFingerprint, WorkspaceError> {
    let mut args = vec![
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "submodule.recurse=false",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
        "--no-renames",
        "--binary",
        "--full-index",
    ];
    if staged {
        args.push("--cached");
        if let GitHeadState::Commit { oid, .. } = head {
            args.extend(["--end-of-options", oid.as_str()]);
        }
    }
    let bytes = run_git_bytes(workspace, &args, deadline)?;
    Ok(GitOutputFingerprint::from_bytes(&bytes))
}

fn capture_diff_summary(
    workspace: &Workspace,
    head: &GitHeadState,
    staged: bool,
    deadline: &ReviewDeadline,
) -> Result<String, WorkspaceError> {
    let mut args = vec![
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "submodule.recurse=false",
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
        "--no-renames",
        "--shortstat",
    ];
    if staged {
        args.push("--cached");
        if let GitHeadState::Commit { oid, .. } = head {
            args.extend(["--end-of-options", oid.as_str()]);
        }
    }
    run_git(workspace, &args, deadline)
}

fn capture_untracked_fingerprints(
    workspace: &Workspace,
    status_entries: &[(String, Utf8PathBuf)],
    deadline: &ReviewDeadline,
) -> Result<Vec<UntrackedEntryFingerprint>, WorkspaceError> {
    let mut total_bytes = 0_u64;
    let mut fingerprints = Vec::new();
    for (_, path) in status_entries.iter().filter(|(code, _)| code == "??") {
        deadline.ensure_remaining("fingerprinting untracked files")?;
        fingerprints.push(capture_untracked_entry(
            workspace,
            path,
            &mut total_bytes,
            deadline,
        )?);
    }
    fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(fingerprints)
}

fn validate_untracked_relative_path(path: &Utf8Path) -> Result<(), WorkspaceError> {
    if path.as_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Utf8Component::Normal(_)))
    {
        return Err(WorkspaceError::Message(format!(
            "git reported unsafe untracked path `{path}`"
        )));
    }
    Ok(())
}

fn open_untracked_parent(
    workspace: &Workspace,
    path: &Utf8Path,
) -> Result<(std::fs::File, Utf8PathBuf, OsString), WorkspaceError> {
    validate_untracked_relative_path(path)?;
    let name = path.file_name().ok_or_else(|| {
        WorkspaceError::Message(format!("untracked path `{path}` has no final component"))
    })?;
    let parent_relative = path.parent().unwrap_or(Utf8Path::new(""));
    let parent = workspace.root.join(parent_relative);
    let guarded_parent = PathGuard::trusted_internal_path(&parent, &workspace.root)?;
    let parent_handle = PathGuard::open_validated_metadata_handle(&guarded_parent)?;
    if !parent_handle.metadata()?.is_dir() {
        return Err(WorkspaceError::Message(format!(
            "untracked parent `{parent}` is not a directory"
        )));
    }
    Ok((
        parent_handle,
        workspace.root.join(path),
        OsString::from(name),
    ))
}

fn reserve_untracked_bytes(
    total_bytes: &mut u64,
    bytes: u64,
    path: &Utf8Path,
) -> Result<(), WorkspaceError> {
    let next = total_bytes.checked_add(bytes).ok_or_else(|| {
        WorkspaceError::Message("untracked review byte count overflowed".to_string())
    })?;
    if next > GIT_REVIEW_UNTRACKED_BYTES_LIMIT {
        return Err(WorkspaceError::Message(format!(
            "untracked review content exceeded the bounded {} byte limit while reading `{path}`",
            GIT_REVIEW_UNTRACKED_BYTES_LIMIT
        )));
    }
    *total_bytes = next;
    Ok(())
}

fn hash_regular_untracked_file(
    file: &mut std::fs::File,
    path: &Utf8Path,
    total_bytes: &mut u64,
    deadline: &ReviewDeadline,
) -> Result<(u64, [u8; 32]), WorkspaceError> {
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(WorkspaceError::Message(format!(
            "unsupported untracked filesystem object `{path}`"
        )));
    }
    reserve_untracked_bytes(total_bytes, before.len(), path)?;
    let first = hash_file_pass(file, path, deadline)?;
    file.seek(SeekFrom::Start(0))?;
    let second = hash_file_pass(file, path, deadline)?;
    let after = file.metadata()?;
    if first.0 != before.len()
        || second.0 != before.len()
        || after.len() != before.len()
        || first.1 != second.1
    {
        return Err(WorkspaceError::Message(format!(
            "untracked file `{path}` changed while it was fingerprinted"
        )));
    }
    Ok((before.len(), first.1))
}

fn hash_file_pass(
    file: &mut std::fs::File,
    path: &Utf8Path,
    deadline: &ReviewDeadline,
) -> Result<(u64, [u8; 32]), WorkspaceError> {
    let mut hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        deadline.ensure_remaining(&format!("reading untracked file `{path}`"))?;
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        byte_len = byte_len
            .checked_add(u64::try_from(read).expect("read size fits u64"))
            .ok_or_else(|| {
                WorkspaceError::Message(format!("untracked file `{path}` is too large"))
            })?;
        if byte_len > GIT_REVIEW_UNTRACKED_BYTES_LIMIT {
            return Err(WorkspaceError::Message(format!(
                "untracked file `{path}` exceeded the bounded review byte limit"
            )));
        }
        hasher.update(&chunk[..read]);
    }
    Ok((byte_len, hasher.finalize().into()))
}

#[cfg(windows)]
fn capture_untracked_entry(
    workspace: &Workspace,
    path: &Utf8Path,
    total_bytes: &mut u64,
    deadline: &ReviewDeadline,
) -> Result<UntrackedEntryFingerprint, WorkspaceError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo,
        GetFileInformationByHandleEx,
    };

    let (parent_handle, absolute, _) = open_untracked_parent(workspace, path)?;
    let opened_parent = PathGuard::opened_file_identity_path(&parent_handle)?;
    let mut file = open_windows_untracked_entry(&absolute, path)?;
    let opened = PathGuard::opened_file_identity_path(&file)?;
    let opened_parent_after = opened.parent().ok_or_else(|| {
        WorkspaceError::Message(format!("opened untracked entry `{path}` has no parent"))
    })?;
    if !PathGuard::same_path_identity(&opened_parent, opened_parent_after) {
        return Err(WorkspaceError::Message(format!(
            "untracked entry `{path}` changed its stable parent while being opened"
        )));
    }
    let identity = UntrackedEntryIdentity::Opened(PathGuard::opened_object_identity(&file)?);
    let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
    let attribute_result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileAttributeTagInfo,
            (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                .expect("attribute tag info size fits u32"),
        )
    };
    if attribute_result == 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to classify untracked entry `{path}`: {}",
            std::io::Error::last_os_error()
        )));
    }

    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let raw = read_windows_reparse_data(&file, path, deadline)?;
        let verify_raw = read_windows_reparse_data(&file, path, deadline)?;
        if raw != verify_raw {
            return Err(WorkspaceError::Message(format!(
                "untracked link `{path}` changed while it was fingerprinted"
            )));
        }
        let byte_len = u64::try_from(raw.len()).expect("link byte length fits u64");
        reserve_untracked_bytes(total_bytes, byte_len, path)?;
        return Ok(UntrackedEntryFingerprint {
            path: path.to_path_buf(),
            kind: UntrackedEntryKind::SymbolicLink,
            identity,
            byte_len,
            sha256: Sha256::digest(&raw).into(),
        });
    }

    let (byte_len, sha256) = hash_regular_untracked_file(&mut file, path, total_bytes, deadline)?;
    Ok(UntrackedEntryFingerprint {
        path: path.to_path_buf(),
        kind: UntrackedEntryKind::RegularFile,
        identity,
        byte_len,
        sha256,
    })
}

#[cfg(windows)]
fn open_windows_untracked_entry(
    absolute: &Utf8Path,
    path: &Utf8Path,
) -> Result<std::fs::File, WorkspaceError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(GENERIC_READ)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    options.open(absolute).map_err(|error| {
        WorkspaceError::Message(format!(
            "failed to open untracked entry `{path}` without following reparse points: {error}"
        ))
    })
}

#[cfg(windows)]
fn read_windows_reparse_data(
    file: &std::fs::File,
    path: &Utf8Path,
    deadline: &ReviewDeadline,
) -> Result<Vec<u8>, WorkspaceError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00a8;
    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;
    const REPARSE_DATA_HEADER_SIZE: usize = 8;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;

    deadline.ensure_remaining(&format!("reading untracked link `{path}`"))?;
    let mut raw = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut returned = 0_u32;
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            raw.as_mut_ptr().cast(),
            u32::try_from(raw.len()).expect("reparse buffer length fits u32"),
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to read untracked link `{path}` from its stable handle: {}",
            std::io::Error::last_os_error()
        )));
    }
    let returned = usize::try_from(returned).expect("reparse byte count fits usize");
    if returned < REPARSE_DATA_HEADER_SIZE {
        return Err(WorkspaceError::Message(format!(
            "untracked link `{path}` returned a truncated reparse header"
        )));
    }
    let tag = u32::from_le_bytes(raw[0..4].try_into().expect("reparse tag is four bytes"));
    if !matches!(tag, IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK) {
        return Err(WorkspaceError::Message(format!(
            "unsupported untracked reparse-point tag 0x{tag:08x} at `{path}`"
        )));
    }
    let data_len = usize::from(u16::from_le_bytes(
        raw[4..6]
            .try_into()
            .expect("reparse data length is two bytes"),
    ));
    let total_len = REPARSE_DATA_HEADER_SIZE
        .checked_add(data_len)
        .ok_or_else(|| WorkspaceError::Message("reparse data length overflowed".to_string()))?;
    if total_len > returned {
        return Err(WorkspaceError::Message(format!(
            "untracked link `{path}` returned truncated reparse data"
        )));
    }
    raw.truncate(total_len);
    Ok(raw)
}

#[cfg(unix)]
fn capture_untracked_entry(
    workspace: &Workspace,
    path: &Utf8Path,
    total_bytes: &mut u64,
    deadline: &ReviewDeadline,
) -> Result<UntrackedEntryFingerprint, WorkspaceError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let (parent_handle, _absolute, name) = open_untracked_parent(workspace, path)?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        WorkspaceError::Message(format!("untracked path `{path}` contains a NUL byte"))
    })?;
    let descriptor = unsafe {
        libc::openat(
            parent_handle.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor >= 0 {
        let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let identity = UntrackedEntryIdentity::Opened(PathGuard::opened_object_identity(&file)?);
        let (byte_len, sha256) =
            hash_regular_untracked_file(&mut file, path, total_bytes, deadline)?;
        return Ok(UntrackedEntryFingerprint {
            path: path.to_path_buf(),
            kind: UntrackedEntryKind::RegularFile,
            identity,
            byte_len,
            sha256,
        });
    }
    let open_error = std::io::Error::last_os_error();
    if open_error.raw_os_error() != Some(libc::ELOOP) {
        return Err(WorkspaceError::Message(format!(
            "failed to open untracked entry `{path}` without following links: {open_error}"
        )));
    }

    let link_handle = open_unix_symlink_handle(parent_handle.as_raw_fd(), &name, path)?;
    let link_identity =
        UntrackedEntryIdentity::Opened(PathGuard::opened_object_identity(&link_handle)?);
    let before = unix_symlink_stat(parent_handle.as_raw_fd(), &name, path)?;
    use std::os::unix::fs::MetadataExt as _;
    let pinned = link_handle.metadata()?;
    if before.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Err(WorkspaceError::Message(format!(
            "untracked entry `{path}` changed after no-follow open"
        )));
    }
    if before.st_dev as u64 != pinned.dev() || before.st_ino as u64 != pinned.ino() {
        return Err(WorkspaceError::Message(format!(
            "untracked link `{path}` changed before its stable handle was verified"
        )));
    }
    let raw = read_unix_link_at(parent_handle.as_raw_fd(), &name, path, deadline)?;
    let verify_raw = read_unix_link_at(parent_handle.as_raw_fd(), &name, path, deadline)?;
    let after = unix_symlink_stat(parent_handle.as_raw_fd(), &name, path)?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_mode != after.st_mode
        || before.st_size != after.st_size
        || raw != verify_raw
    {
        return Err(WorkspaceError::Message(format!(
            "untracked link `{path}` changed while it was fingerprinted"
        )));
    }
    let byte_len = u64::try_from(raw.len()).expect("link byte length fits u64");
    reserve_untracked_bytes(total_bytes, byte_len, path)?;
    Ok(UntrackedEntryFingerprint {
        path: path.to_path_buf(),
        kind: UntrackedEntryKind::SymbolicLink,
        identity: link_identity,
        byte_len,
        sha256: Sha256::digest(&raw).into(),
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_unix_symlink_handle(
    parent_fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    path: &Utf8Path,
) -> Result<std::fs::File, WorkspaceError> {
    use std::os::fd::FromRawFd as _;

    let descriptor = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to pin untracked link `{path}` without following it: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(target_vendor = "apple")]
const APPLE_SYMLINK_OPEN_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_SYMLINK | libc::O_CLOEXEC;

#[cfg(target_vendor = "apple")]
const _: [libc::c_int; 4] = [
    libc::O_RDONLY,
    libc::O_SYMLINK,
    libc::O_CLOEXEC,
    APPLE_SYMLINK_OPEN_FLAGS,
];

#[cfg(target_vendor = "apple")]
fn open_unix_symlink_handle(
    parent_fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    path: &Utf8Path,
) -> Result<std::fs::File, WorkspaceError> {
    use std::os::fd::FromRawFd as _;

    let descriptor = unsafe { libc::openat(parent_fd, name.as_ptr(), APPLE_SYMLINK_OPEN_FLAGS) };
    if descriptor < 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to pin untracked link `{path}` without following it: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn open_unix_symlink_handle(
    _parent_fd: std::os::fd::RawFd,
    _name: &std::ffi::CStr,
    path: &Utf8Path,
) -> Result<std::fs::File, WorkspaceError> {
    Err(WorkspaceError::Message(format!(
        "untracked link `{path}` cannot be reviewed because this Unix host has no supported stable no-follow link handle"
    )))
}

#[cfg(unix)]
fn unix_symlink_stat(
    parent_fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    path: &Utf8Path,
) -> Result<libc::stat, WorkspaceError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to identify untracked link `{path}`: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(unix)]
fn read_unix_link_at(
    parent_fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    path: &Utf8Path,
    deadline: &ReviewDeadline,
) -> Result<Vec<u8>, WorkspaceError> {
    deadline.ensure_remaining(&format!("reading untracked link `{path}`"))?;
    let mut bytes = vec![0_u8; 64 * 1024];
    let read = unsafe {
        libc::readlinkat(
            parent_fd,
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if read < 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to read untracked link `{path}`: {}",
            std::io::Error::last_os_error()
        )));
    }
    let read = usize::try_from(read).expect("nonnegative link byte count fits usize");
    if read == bytes.len() {
        return Err(WorkspaceError::Message(format!(
            "untracked link `{path}` exceeded the bounded link-target limit"
        )));
    }
    bytes.truncate(read);
    Ok(bytes)
}

#[cfg(not(any(windows, unix)))]
fn capture_untracked_entry(
    workspace: &Workspace,
    path: &Utf8Path,
    total_bytes: &mut u64,
    deadline: &ReviewDeadline,
) -> Result<UntrackedEntryFingerprint, WorkspaceError> {
    let (_parent_handle, absolute, _) = open_untracked_parent(workspace, path)?;
    let guarded = PathGuard::trusted_internal_path(&absolute, &workspace.root)?;
    let mut file = PathGuard::open_validated_read_file(&guarded)?;
    let identity = UntrackedEntryIdentity::Opened(PathGuard::opened_object_identity(&file)?);
    let (byte_len, sha256) = hash_regular_untracked_file(&mut file, path, total_bytes, deadline)?;
    Ok(UntrackedEntryFingerprint {
        path: path.to_path_buf(),
        kind: UntrackedEntryKind::RegularFile,
        identity,
        byte_len,
        sha256,
    })
}

fn run_git(
    workspace: &Workspace,
    args: &[&str],
    deadline: &ReviewDeadline,
) -> Result<String, WorkspaceError> {
    let output = run_git_bytes(workspace, args, deadline)?;
    String::from_utf8(output).map_err(|error| {
        WorkspaceError::Message(format!(
            "git {} returned non-UTF-8 output: {error}",
            args.join(" ")
        ))
    })
}

fn run_git_bytes(
    workspace: &Workspace,
    args: &[&str],
    deadline: &ReviewDeadline,
) -> Result<Vec<u8>, WorkspaceError> {
    let output = capture_git_command(workspace, args, deadline)?;
    if !output.status.success() {
        return Err(git_command_failure(args, &output));
    }
    Ok(output.stdout.bytes)
}

fn capture_git_command(
    workspace: &Workspace,
    args: &[&str],
    deadline: &ReviewDeadline,
) -> Result<GitCommandCapture, WorkspaceError> {
    deadline.ensure_remaining(&format!("starting git {}", args.join(" ")))?;
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(workspace.root.as_std_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_git_environment(&mut command);
    configure_git_process_tree(&mut command);
    let mut child = command.spawn().map_err(|error| {
        WorkspaceError::Message(format!("failed to run git {}: {error}", args.join(" ")))
    })?;
    let process_tree = GitProcessTree::attach(&mut child).map_err(|error| {
        let _ = child.kill();
        let _ = child.wait();
        WorkspaceError::Message(format!(
            "failed to establish process-tree ownership for git {}: {error}",
            args.join(" ")
        ))
    })?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_git_child(&mut child, &process_tree);
            return Err(WorkspaceError::Message(
                "failed to capture git review stdout".to_string(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_git_child(&mut child, &process_tree);
            return Err(WorkspaceError::Message(
                "failed to capture git review stderr".to_string(),
            ));
        }
    };
    if let Err(error) = configure_git_pipe_reader(&stdout) {
        terminate_git_child(&mut child, &process_tree);
        return Err(WorkspaceError::Message(format!(
            "failed to configure git stdout reader: {error}"
        )));
    }
    if let Err(error) = configure_git_pipe_reader(&stderr) {
        terminate_git_child(&mut child, &process_tree);
        return Err(WorkspaceError::Message(format!(
            "failed to configure git stderr reader: {error}"
        )));
    }
    let stdout_reader = spawn_bounded_reader(stdout, GIT_REVIEW_STDOUT_LIMIT_BYTES, *deadline);
    let stderr_reader = spawn_bounded_reader(stderr, GIT_REVIEW_STDERR_LIMIT_BYTES, *deadline);
    let status = wait_for_git_exit(&mut child, &process_tree, deadline, args)?;
    drop(process_tree);
    let stdout = receive_bounded_reader(stdout_reader, "stdout", deadline)?;
    let stderr = receive_bounded_reader(stderr_reader, "stderr", deadline)?;
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
    Ok(GitCommandCapture {
        status,
        stdout,
        stderr,
    })
}

fn git_command_failure(args: &[&str], output: &GitCommandCapture) -> WorkspaceError {
    let stderr = String::from_utf8_lossy(&output.stderr.bytes)
        .trim()
        .to_string();
    let stdout = String::from_utf8_lossy(&output.stdout.bytes)
        .trim()
        .to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    WorkspaceError::Message(format!(
        "git {} failed: {}",
        args.join(" "),
        if detail.is_empty() {
            output.status.to_string()
        } else {
            detail
        }
    ))
}

fn configure_git_environment(command: &mut Command) {
    let inherited = environment_without_git(std::env::vars_os());
    command.env_clear();
    command.envs(inherited);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat");
}

fn is_git_environment_key(key: &std::ffi::OsStr) -> bool {
    key.to_string_lossy()
        .as_bytes()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
}

fn environment_without_git(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    environment
        .into_iter()
        .filter(|(key, _)| !is_git_environment_key(key))
        .collect()
}

fn terminate_git_child(child: &mut std::process::Child, process_tree: &GitProcessTree) {
    let _ = process_tree.terminate();
    let _ = child.kill();
    let _ = child.wait();
}

struct BoundedReader {
    receiver: Receiver<std::io::Result<BoundedStreamCapture>>,
    cancelled: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BoundedReader {
    fn cancel_and_join(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(thread) = self.thread.as_ref() {
            interrupt_reader_thread(thread);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BoundedReader {
    fn drop(&mut self) {
        self.cancel_and_join();
    }
}

fn spawn_bounded_reader(
    stream: impl Read + Send + 'static,
    limit: usize,
    deadline: ReviewDeadline,
) -> BoundedReader {
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let reader_cancelled = Arc::clone(&cancelled);
    let thread = thread::spawn(move || {
        let _ = sender.send(read_bounded_stream_cancellable(
            stream,
            limit,
            &reader_cancelled,
            &deadline,
        ));
    });
    BoundedReader {
        receiver,
        cancelled,
        thread: Some(thread),
    }
}

fn receive_bounded_reader(
    mut reader: BoundedReader,
    stream_name: &str,
    deadline: &ReviewDeadline,
) -> Result<BoundedStreamCapture, WorkspaceError> {
    let remaining = deadline.remaining(&format!("waiting for git {stream_name}"))?;
    let result = match reader.receiver.recv_timeout(remaining) {
        Ok(Ok(capture)) => Ok(capture),
        Ok(Err(error)) => Err(WorkspaceError::Message(format!(
            "failed to read bounded git review {stream_name}: {error}"
        ))),
        Err(RecvTimeoutError::Timeout) => Err(WorkspaceError::Message(format!(
            "git review {stream_name} pipe did not close before the review-operation deadline"
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(WorkspaceError::Message(format!(
            "git review {stream_name} reader stopped without a result"
        ))),
    };
    reader.cancel_and_join();
    result
}

#[cfg(unix)]
fn configure_git_pipe_reader(stream: &impl std::os::fd::AsRawFd) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd as _;

    let descriptor = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_git_pipe_reader<T>(_stream: &T) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(windows)]
fn interrupt_reader_thread(thread: &thread::JoinHandle<()>) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::CancelSynchronousIo;

    unsafe {
        CancelSynchronousIo(thread.as_raw_handle() as HANDLE);
    }
}

#[cfg(not(windows))]
fn interrupt_reader_thread(_thread: &thread::JoinHandle<()>) {}

#[cfg(unix)]
fn wait_for_git_exit(
    child: &mut std::process::Child,
    process_tree: &GitProcessTree,
    deadline: &ReviewDeadline,
    args: &[&str],
) -> Result<std::process::ExitStatus, WorkspaceError> {
    loop {
        match unix_child_exit_is_observable(child.id()) {
            Ok(true) => {
                let termination = process_tree.terminate();
                let status = child.wait().map_err(|error| {
                    WorkspaceError::Message(format!(
                        "failed to reap git {} after observing its exit: {error}",
                        args.join(" ")
                    ))
                })?;
                termination.map_err(|error| {
                    WorkspaceError::Message(format!(
                        "failed to close descendant process ownership for git {}: {error}",
                        args.join(" ")
                    ))
                })?;
                return Ok(status);
            }
            Ok(false) => match deadline.remaining(&format!("waiting for git {}", args.join(" "))) {
                Ok(remaining) => thread::sleep(GIT_REVIEW_WAIT_POLL.min(remaining)),
                Err(error) => {
                    let _ = process_tree.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = process_tree.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorkspaceError::Message(format!(
                    "failed while observing git {} without reaping its group leader: {error}",
                    args.join(" ")
                )));
            }
        }
    }
}

#[cfg(unix)]
fn unix_child_exit_is_observable(pid: u32) -> Result<bool, std::io::Error> {
    let pid = libc::id_t::try_from(pid)
        .map_err(|_| std::io::Error::other("git process id does not fit waitid"))?;
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.si_signo != 0)
}

#[cfg(not(unix))]
fn wait_for_git_exit(
    child: &mut std::process::Child,
    process_tree: &GitProcessTree,
    deadline: &ReviewDeadline,
    args: &[&str],
) -> Result<std::process::ExitStatus, WorkspaceError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                process_tree.terminate().map_err(|error| {
                    WorkspaceError::Message(format!(
                        "failed to close descendant process ownership for git {}: {error}",
                        args.join(" ")
                    ))
                })?;
                return Ok(status);
            }
            Ok(None) => match deadline.remaining(&format!("waiting for git {}", args.join(" "))) {
                Ok(remaining) => thread::sleep(GIT_REVIEW_WAIT_POLL.min(remaining)),
                Err(error) => {
                    let _ = process_tree.terminate();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            },
            Err(error) => {
                let _ = process_tree.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorkspaceError::Message(format!(
                    "failed while waiting for git {}: {error}",
                    args.join(" ")
                )));
            }
        }
    }
}

#[cfg(unix)]
fn configure_git_process_tree(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
}

#[cfg(windows)]
fn configure_git_process_tree(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn configure_git_process_tree(_command: &mut Command) {}

#[cfg(unix)]
struct GitProcessTree {
    process_group: i32,
}

#[cfg(unix)]
impl GitProcessTree {
    fn attach(child: &mut std::process::Child) -> std::io::Result<Self> {
        let process_group = i32::try_from(child.id()).map_err(|_| {
            std::io::Error::other("git process id does not fit the Unix process-group type")
        })?;
        Ok(Self { process_group })
    }

    fn terminate(&self) -> std::io::Result<()> {
        // SAFETY: `process_group` was created as the child's own process group before spawn.
        let result = unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(unix)]
impl Drop for GitProcessTree {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(windows)]
struct GitProcessTree {
    handle: isize,
}

#[cfg(windows)]
impl GitProcessTree {
    fn attach(child: &mut std::process::Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("job limit information size fits u32"),
            )
        };
        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        let tree = Self {
            handle: handle as isize,
        };
        if let Err(error) = resume_windows_git_process(child.id()) {
            drop(tree);
            return Err(error);
        }
        Ok(tree)
    }

    fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if unsafe { TerminateJobObject(self.handle as HANDLE, 1) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for GitProcessTree {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

        let _ = self.terminate();
        unsafe {
            CloseHandle(self.handle as HANDLE);
        }
    }
}

#[cfg(windows)]
fn resume_windows_git_process(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(std::mem::size_of::<THREADENTRY32>())
            .expect("thread entry size fits u32"),
        ..Default::default()
    };
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let result = loop {
        if !has_entry {
            break Err(std::io::Error::other(format!(
                "suspended git process {pid} has no resumable primary thread"
            )));
        }
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                break Err(std::io::Error::last_os_error());
            }
            let resume_result = unsafe { ResumeThread(thread) };
            unsafe {
                CloseHandle(thread);
            }
            if resume_result == u32::MAX {
                break Err(std::io::Error::last_os_error());
            }
            break Ok(());
        }
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    };
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

#[cfg(not(any(unix, windows)))]
struct GitProcessTree;

#[cfg(not(any(unix, windows)))]
impl GitProcessTree {
    fn attach(_child: &mut std::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    fn terminate(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct GitCommandCapture {
    status: std::process::ExitStatus,
    stdout: BoundedStreamCapture,
    stderr: BoundedStreamCapture,
}

#[derive(Debug)]
struct BoundedStreamCapture {
    bytes: Vec<u8>,
    overflowed: bool,
}

#[cfg(test)]
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

fn read_bounded_stream_cancellable(
    mut stream: impl Read,
    limit: usize,
    cancelled: &AtomicBool,
    deadline: &ReviewDeadline,
) -> std::io::Result<BoundedStreamCapture> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut overflowed = false;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "git review reader was cancelled",
            ));
        }
        let remaining = deadline
            .remaining("draining a git output pipe")
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, error.to_string())
            })?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let retained = limit.saturating_sub(bytes.len()).min(read);
                bytes.extend_from_slice(&chunk[..retained]);
                overflowed |= retained < read;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(GIT_REVIEW_WAIT_POLL.min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(BoundedStreamCapture { bytes, overflowed })
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
    use std::process::Command;

    use super::*;
    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    fn run_fixture_git(root: &camino::Utf8Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run fixture git command");
        assert!(
            status.success(),
            "git {} failed with {status}",
            args.join(" ")
        );
    }

    fn git_workspace() -> (tempfile::TempDir, Workspace) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        run_fixture_git(&root, &["init", "--quiet"]);
        run_fixture_git(&root, &["config", "user.email", "moyai@example.invalid"]);
        run_fixture_git(&root, &["config", "user.name", "moyAI Test"]);
        std::fs::write(root.join("tracked.txt"), "baseline\n").expect("seed tracked file");
        run_fixture_git(&root, &["add", "tracked.txt"]);
        run_fixture_git(&root, &["commit", "--quiet", "-m", "baseline"]);
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("git workspace");
        (temp, workspace)
    }

    fn unborn_git_workspace() -> (tempfile::TempDir, Workspace) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        run_fixture_git(&root, &["init", "--quiet"]);
        run_fixture_git(&root, &["config", "user.email", "moyai@example.invalid"]);
        run_fixture_git(&root, &["config", "user.name", "moyAI Test"]);
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("unborn git workspace");
        (temp, workspace)
    }

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

    #[test]
    fn uncommitted_review_rejects_a_state_change_between_inventory_reads() {
        let (_temp, workspace) = git_workspace();
        let changed = workspace.root.join("tracked.txt");

        let error = uncommitted_review_scope_with_observer(&workspace, || {
            std::fs::write(&changed, "changed during review\n").expect("change tracked file");
        })
        .expect_err("a moving worktree must not produce a mixed review scope");

        assert!(error.to_string().contains("state changed"));
    }

    #[test]
    fn uncommitted_review_rejects_same_shape_tracked_content_swap() {
        let (_temp, workspace) = git_workspace();
        let changed = workspace.root.join("tracked.txt");
        std::fs::write(&changed, "content-a\n").expect("first same-shape content");

        let error = uncommitted_review_scope_with_observer(&workspace, || {
            std::fs::write(&changed, "content-b\n").expect("second same-shape content");
        })
        .expect_err("an exact tracked-content swap must invalidate the review scope");

        assert!(error.to_string().contains("state changed"));
    }

    #[test]
    fn uncommitted_review_rejects_same_shape_change_between_capture_commands() {
        let (_temp, workspace) = git_workspace();
        let changed = workspace.root.join("tracked.txt");
        std::fs::write(&changed, "content-a\n").expect("first same-shape content");

        let error = uncommitted_review_scope_with_capture_observer(&workspace, || {
            std::fs::write(&changed, "content-b\n").expect("second same-shape content");
        })
        .expect_err("a command-between content swap must invalidate the review scope");

        assert!(error.to_string().contains("state changed"));
    }

    #[test]
    fn uncommitted_review_rejects_same_shape_untracked_replacement() {
        let (_temp, workspace) = git_workspace();
        let untracked = workspace.root.join("untracked.txt");
        let displaced = workspace.root.join("untracked.old");
        std::fs::write(&untracked, "same-bytes\n").expect("seed untracked file");

        let error = uncommitted_review_scope_with_observer(&workspace, || {
            std::fs::rename(&untracked, &displaced).expect("replace untracked identity");
            std::fs::write(&untracked, "same-bytes\n").expect("replacement untracked file");
        })
        .expect_err("an untracked identity replacement must invalidate the review scope");

        assert!(error.to_string().contains("state changed"));
    }

    #[test]
    fn uncommitted_review_rejects_untracked_link_replacement() {
        let (_temp, workspace) = git_workspace();
        let link = workspace.root.join("untracked-link");
        if !create_test_file_link("target-a", &link) {
            return;
        }

        let error = uncommitted_review_scope_with_observer(&workspace, || {
            std::fs::remove_file(&link).expect("remove first untracked link");
            assert!(create_test_file_link("target-b", &link));
        })
        .expect_err("an untracked link replacement must invalidate the review scope");

        assert!(error.to_string().contains("state changed"));
    }

    #[cfg(unix)]
    fn create_test_file_link(target: &str, link: &Utf8Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("create Unix test link");
        true
    }

    #[cfg(windows)]
    fn create_test_file_link(target: &str, link: &Utf8Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) =>
            {
                false
            }
            Err(error) => panic!("create Windows test link: {error}"),
        }
    }

    #[cfg(not(any(windows, unix)))]
    fn create_test_file_link(_target: &str, _link: &Utf8Path) -> bool {
        false
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_fingerprint_stays_bound_to_the_opened_entry() {
        let (_temp, workspace) = git_workspace();
        let link_a = Utf8Path::new("link-a");
        let link_b = Utf8Path::new("link-b");
        if !create_test_file_link("target-a", &workspace.root.join(link_a)) {
            return;
        }
        assert!(create_test_file_link(
            "target-b",
            &workspace.root.join(link_b)
        ));
        let opened_a = open_windows_untracked_entry(&workspace.root.join(link_a), link_a)
            .expect("open first link handle");
        let opened_b = open_windows_untracked_entry(&workspace.root.join(link_b), link_b)
            .expect("open second link handle");
        let deadline = ReviewDeadline::new(GIT_REVIEW_OPERATION_TIMEOUT);
        let first = read_windows_reparse_data(&opened_a, link_b, &deadline)
            .expect("read first handle with another path used only for diagnostics");

        assert_eq!(
            read_windows_reparse_data(&opened_a, link_a, &deadline).expect("reread first handle"),
            first
        );
        assert_ne!(
            read_windows_reparse_data(&opened_b, link_b, &deadline).expect("read second handle"),
            first
        );
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_symlink_handle_stays_bound_after_namespace_replacement() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;

        let (_temp, workspace) = git_workspace();
        let path = Utf8Path::new("pinned-link");
        let link = workspace.root.join(path);
        assert!(create_test_file_link("target-a", &link));
        let (parent_handle, _absolute, name) =
            open_untracked_parent(&workspace, path).expect("open stable parent");
        let name = CString::new(name.as_bytes()).expect("single link component");
        let pinned = open_unix_symlink_handle(parent_handle.as_raw_fd(), &name, path)
            .expect("pin Apple symlink handle");
        let before =
            unix_symlink_stat(parent_handle.as_raw_fd(), &name, path).expect("identify first link");
        let pinned_before = pinned.metadata().expect("first pinned metadata");
        assert!(pinned_before.file_type().is_symlink());
        assert_eq!(before.st_dev as u64, pinned_before.dev());
        assert_eq!(before.st_ino as u64, pinned_before.ino());
        let descriptor_flags = unsafe { libc::fcntl(pinned.as_raw_fd(), libc::F_GETFD) };
        assert!(descriptor_flags >= 0, "read descriptor flags");
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        let deadline = ReviewDeadline::new(GIT_REVIEW_OPERATION_TIMEOUT);
        let first_target = read_unix_link_at(parent_handle.as_raw_fd(), &name, path, &deadline)
            .expect("read first link target");
        assert_eq!(first_target.as_slice(), b"target-a");

        std::fs::remove_file(&link).expect("remove first link");
        assert!(create_test_file_link("target-b", &link));
        let after = unix_symlink_stat(parent_handle.as_raw_fd(), &name, path)
            .expect("identify replacement link");
        let pinned_after = pinned.metadata().expect("replacement pinned metadata");

        assert_eq!(pinned_before.dev(), pinned_after.dev());
        assert_eq!(pinned_before.ino(), pinned_after.ino());
        assert_ne!(
            (before.st_dev as u64, before.st_ino as u64),
            (after.st_dev as u64, after.st_ino as u64)
        );
        let replacement_target =
            read_unix_link_at(parent_handle.as_raw_fd(), &name, path, &deadline)
                .expect("read replacement link target");
        assert_eq!(replacement_target.as_slice(), b"target-b");
    }

    #[test]
    fn uncommitted_review_supports_staged_and_untracked_files_on_unborn_head() {
        let (_temp, workspace) = unborn_git_workspace();
        std::fs::write(workspace.root.join("staged.txt"), "staged\n").expect("staged file");
        std::fs::write(workspace.root.join("untracked.txt"), "untracked\n")
            .expect("untracked file");
        run_fixture_git(&workspace.root, &["add", "staged.txt"]);

        let scope = uncommitted_review_scope(&workspace).expect("unborn review scope");

        assert_eq!(scope.mode, ReviewScopeMode::Uncommitted);
        assert!(
            scope
                .head_ref
                .as_deref()
                .is_some_and(|head| !head.is_empty())
        );
        assert_eq!(
            scope.changed_files,
            vec![
                Utf8PathBuf::from("staged.txt"),
                Utf8PathBuf::from("untracked.txt")
            ]
        );
        assert!(scope.summary.contains("staged:"));
        assert!(scope.summary.contains("untracked: 1 file(s)"));
    }

    #[test]
    fn symbolic_head_with_an_existing_noncommit_ref_is_not_unborn() {
        let (_temp, workspace) = git_workspace();
        let deadline = ReviewDeadline::new(GIT_REVIEW_OPERATION_TIMEOUT);
        let blob = run_git(&workspace, &["hash-object", "tracked.txt"], &deadline)
            .expect("tracked blob identity");
        run_fixture_git(
            &workspace.root,
            &["symbolic-ref", "HEAD", "refs/heads/noncommit"],
        );
        std::fs::write(
            workspace.root.join(".git/refs/heads/noncommit"),
            format!("{}\n", blob.trim()),
        )
        .expect("write noncommit branch ref");

        let error = resolve_head_state(&workspace, &deadline)
            .expect_err("an existing noncommit branch ref must fail closed");

        assert!(error.to_string().contains("commit"));
    }

    #[test]
    fn git_environment_filter_removes_all_git_prefixed_values() {
        let filtered = environment_without_git([
            (OsString::from("PATH"), OsString::from("fixture-path")),
            (OsString::from("GIT_DIR"), OsString::from("outside.git")),
            (
                OsString::from("git_work_tree"),
                OsString::from("outside-worktree"),
            ),
            (
                OsString::from("GIT_INDEX_FILE"),
                OsString::from("outside-index"),
            ),
        ]);

        assert_eq!(
            filtered,
            vec![(OsString::from("PATH"), OsString::from("fixture-path"))]
        );
    }

    #[test]
    fn review_deadline_is_cumulative_across_git_commands() {
        let (_temp, workspace) = git_workspace();
        let deadline = ReviewDeadline::new(Duration::from_secs(2));
        let _ =
            run_git(&workspace, &["rev-parse", "--git-dir"], &deadline).expect("first git command");
        thread::sleep(
            deadline
                .end
                .saturating_duration_since(Instant::now())
                .saturating_add(Duration::from_millis(25)),
        );

        let error = run_git(&workspace, &["rev-parse", "--git-dir"], &deadline)
            .expect_err("the shared deadline must not reset for a later command");

        assert!(error.to_string().contains("scope deadline"));
    }

    #[test]
    fn branch_review_rejects_refs_that_move_after_inventory_capture() {
        let (_temp, workspace) = git_workspace();
        std::fs::write(workspace.root.join("tracked.txt"), "second\n").expect("second change");
        run_fixture_git(&workspace.root, &["add", "tracked.txt"]);
        run_fixture_git(&workspace.root, &["commit", "--quiet", "-m", "second"]);

        let error = branch_review_scope_with_observer(&workspace, "HEAD~1", || {
            run_fixture_git(
                &workspace.root,
                &["commit", "--quiet", "--allow-empty", "-m", "moving head"],
            );
        })
        .expect_err("moving refs must invalidate a captured branch review");

        assert!(error.to_string().contains("refs changed"));
    }

    struct HeldPipeReader {
        dropped: Arc<AtomicBool>,
    }

    impl Read for HeldPipeReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        }
    }

    impl Drop for HeldPipeReader {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[test]
    fn reader_wait_is_bounded_and_joins_when_an_inherited_pipe_stays_open() {
        let dropped = Arc::new(AtomicBool::new(false));
        let deadline = ReviewDeadline::new(Duration::from_millis(25));
        let reader = spawn_bounded_reader(
            HeldPipeReader {
                dropped: Arc::clone(&dropped),
            },
            16,
            deadline,
        );
        let started = Instant::now();

        let error = receive_bounded_reader(reader, "stdout", &deadline)
            .expect_err("a held pipe must hit the bounded reader deadline");

        assert!(error.to_string().contains("deadline") || error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(dropped.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    struct EscapedProcessCleanup {
        pid_file: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl Drop for EscapedProcessCleanup {
        fn drop(&mut self) {
            let Ok(pid) = std::fs::read_to_string(&self.pid_file) else {
                return;
            };
            let Ok(pid) = pid.trim().parse::<libc::pid_t>() else {
                return;
            };
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn escaped_descendant_pipe_holder_hits_deadline_without_leaving_reader_thread() {
        let setsid_available = Command::new("setsid")
            .arg("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !setsid_available {
            return;
        }

        let temp = tempfile::tempdir().expect("escaped descendant fixture");
        let pid_file = temp.path().join("escaped.pid");
        let _cleanup = EscapedProcessCleanup {
            pid_file: pid_file.clone(),
        };
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "setsid sh -c 'echo $$ > \"$MOYAI_TEST_PID_FILE\"; sleep 5' &",
            ])
            .env("MOYAI_TEST_PID_FILE", &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_git_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn process-group fixture");
        let process_tree = GitProcessTree::attach(&mut child).expect("own fixture process group");
        let stdout = child.stdout.take().expect("fixture stdout");
        let stderr = child.stderr.take().expect("fixture stderr");
        configure_git_pipe_reader(&stdout).expect("nonblocking fixture stdout");
        configure_git_pipe_reader(&stderr).expect("nonblocking fixture stderr");
        let deadline = ReviewDeadline::new(Duration::from_millis(500));
        let stdout_reader = spawn_bounded_reader(stdout, 1024, deadline);
        let stderr_reader = spawn_bounded_reader(stderr, 1024, deadline);

        let started = Instant::now();
        let status = wait_for_git_exit(
            &mut child,
            &process_tree,
            &deadline,
            &["escaped-descendant-fixture"],
        )
        .expect("fixture group leader exits");
        assert!(status.success());
        drop(process_tree);
        let error = receive_bounded_reader(stdout_reader, "stdout", &deadline)
            .expect_err("escaped descendant must hold the inherited pipe until the deadline");
        drop(stderr_reader);

        assert!(error.to_string().contains("deadline") || error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
