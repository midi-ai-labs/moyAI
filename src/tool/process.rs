use std::io;
use std::process::{ExitStatus, Stdio};

use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::tool::truncate::{BoundedPipeOutput, read_pipe_bounded};

const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const CLEANUP_HELPER_TIMEOUT: Duration = Duration::from_millis(1_500);
#[cfg(windows)]
const CLEANUP_HELPER_REAP_TIMEOUT: Duration = Duration::from_millis(500);

/// Cancellation callers must keep the tool future alive for at least this
/// long so process-tree termination, direct-child reaping, and pipe-reader
/// joining can complete. The individual cleanup phases are bounded below this
/// value; the remainder is scheduling margin for a loaded host.
pub(crate) const MANAGED_PROCESS_CLEANUP_GRACE: Duration = Duration::from_secs(12);

/// Owns a subprocess, its process-tree identity, and both output readers until
/// the child has been reaped and the readers have been joined.
///
/// `kill_on_drop` is only a last-resort safety net. Cancellation and timeout
/// paths must consume this value through [`Self::terminate`] so descendants and
/// inherited pipe handles are cleaned up cooperatively before the future ends.
pub(crate) struct ManagedProcess {
    child: Child,
    pid: u32,
    hide_window: bool,
    stdout_task: JoinHandle<Result<BoundedPipeOutput, io::Error>>,
    stderr_task: JoinHandle<Result<BoundedPipeOutput, io::Error>>,
    #[cfg(windows)]
    job: Option<WindowsJob>,
}

#[derive(Debug)]
pub(crate) struct ManagedProcessOutput {
    pub status: Option<ExitStatus>,
    pub stdout: BoundedPipeOutput,
    pub stderr: BoundedPipeOutput,
    pub cleanup_errors: Vec<String>,
}

impl ManagedProcessOutput {
    pub fn cleanup_error(&self) -> Option<String> {
        (!self.cleanup_errors.is_empty()).then(|| self.cleanup_errors.join("; "))
    }
}

impl ManagedProcess {
    pub async fn spawn(
        mut command: Command,
        hide_window: bool,
        max_output_bytes: usize,
    ) -> Result<Self, io::Error> {
        configure_process_group(&mut command, hide_window, true);
        command.kill_on_drop(true);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("spawned process has no process id"))?;
        #[cfg(windows)]
        let (job, startup_error) = match WindowsJob::assign(&child) {
            Ok(job) => match resume_windows_process(pid) {
                Ok(()) => (Some(job), None),
                Err(error) => (Some(job), Some(error)),
            },
            Err(error) => (None, Some(error)),
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("spawned process stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("spawned process stderr was not captured"))?;
        let output_limit = max_output_bytes.max(1);

        let process = Self {
            child,
            pid,
            hide_window,
            stdout_task: tokio::spawn(read_pipe_bounded(stdout, output_limit)),
            stderr_task: tokio::spawn(read_pipe_bounded(stderr, output_limit)),
            #[cfg(windows)]
            job,
        };
        #[cfg(windows)]
        if let Some(startup_error) = startup_error {
            let cleanup = process.terminate().await;
            let cleanup_suffix = cleanup
                .cleanup_error()
                .map(|error| format!("; fallback cleanup failed: {error}"))
                .unwrap_or_default();
            return Err(io::Error::other(format!(
                "failed to establish suspended Windows Job ownership before subprocess execution: {startup_error}{cleanup_suffix}"
            )));
        }
        Ok(process)
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub async fn wait(&mut self) -> Result<ExitStatus, io::Error> {
        self.child.wait().await
    }

    /// Completes the normal-exit lifecycle. Any descendants that retained the
    /// child's pipe handles are stopped before both reader tasks are drained.
    pub async fn finish_after_exit(self, status: ExitStatus) -> ManagedProcessOutput {
        let mut cleanup_errors = Vec::new();
        if let Err(error) = self.cleanup_process_tree_after_parent_exit().await {
            cleanup_errors.push(error.to_string());
        }
        self.drain(Some(status), cleanup_errors).await
    }

    /// Stops the complete process tree, reaps the direct child, and joins both
    /// pipe readers. This method is deliberately consuming: successful return
    /// means no subprocess or reader-task owner was silently dropped.
    pub async fn terminate(mut self) -> ManagedProcessOutput {
        let mut cleanup_errors = Vec::new();
        let mut status = None;

        for step in process_tree_termination_plan() {
            match step {
                ProcessTerminationStep::ProcessTreeKill => {
                    if let Err(error) = self.terminate_owned_process_tree().await {
                        cleanup_errors.push(error.to_string());
                    }
                }
                ProcessTerminationStep::ParentStartKill => {
                    let _ = self.child.start_kill();
                }
                ProcessTerminationStep::WaitForParent => {
                    match timeout(PROCESS_EXIT_TIMEOUT, self.child.wait()).await {
                        Ok(Ok(exit_status)) => status = Some(exit_status),
                        Ok(Err(error)) => cleanup_errors
                            .push(format!("failed to reap subprocess {}: {error}", self.pid)),
                        Err(_) => cleanup_errors.push(format!(
                            "subprocess {} did not exit within {} ms after tree termination",
                            self.pid,
                            PROCESS_EXIT_TIMEOUT.as_millis()
                        )),
                    }
                }
            }
        }
        self.drain(status, cleanup_errors).await
    }

    #[cfg(windows)]
    async fn terminate_owned_process_tree(&self) -> Result<(), io::Error> {
        if let Some(job) = &self.job {
            match job.terminate() {
                Ok(()) => return Ok(()),
                Err(job_error) => {
                    let fallback = kill_windows_process_tree(self.pid, self.hide_window).await;
                    return combine_cleanup_results(Err(job_error), fallback);
                }
            }
        }
        kill_windows_process_tree(self.pid, self.hide_window).await
    }

    #[cfg(unix)]
    async fn terminate_owned_process_tree(&self) -> Result<(), io::Error> {
        kill_process_tree(self.pid, self.hide_window).await
    }

    #[cfg(not(any(unix, windows)))]
    async fn terminate_owned_process_tree(&self) -> Result<(), io::Error> {
        Ok(())
    }

    #[cfg(windows)]
    async fn cleanup_process_tree_after_parent_exit(&self) -> Result<(), io::Error> {
        match &self.job {
            Some(job) => job.terminate(),
            None => Ok(()),
        }
    }

    #[cfg(unix)]
    async fn cleanup_process_tree_after_parent_exit(&self) -> Result<(), io::Error> {
        kill_process_tree(self.pid, self.hide_window).await
    }

    #[cfg(not(any(unix, windows)))]
    async fn cleanup_process_tree_after_parent_exit(&self) -> Result<(), io::Error> {
        Ok(())
    }

    async fn drain(
        self,
        status: Option<ExitStatus>,
        mut cleanup_errors: Vec<String>,
    ) -> ManagedProcessOutput {
        let Self {
            child,
            stdout_task,
            stderr_task,
            ..
        } = self;
        let (stdout, stderr) = tokio::join!(
            drain_pipe_reader(stdout_task, "stdout"),
            drain_pipe_reader(stderr_task, "stderr")
        );
        let (stdout, stdout_error) = stdout;
        let (stderr, stderr_error) = stderr;
        cleanup_errors.extend(stdout_error);
        cleanup_errors.extend(stderr_error);

        // Keep the reaped child handle alive until the inherited pipes have
        // either closed or their reader tasks have been aborted and joined.
        drop(child);

        ManagedProcessOutput {
            status,
            stdout,
            stderr,
            cleanup_errors,
        }
    }
}

async fn drain_pipe_reader(
    mut task: JoinHandle<Result<BoundedPipeOutput, io::Error>>,
    label: &str,
) -> (BoundedPipeOutput, Option<String>) {
    match timeout(PIPE_DRAIN_TIMEOUT, &mut task).await {
        Ok(Ok(Ok(output))) => (output, None),
        Ok(Ok(Err(error))) => (
            empty_pipe_output(),
            Some(format!("failed to read subprocess {label}: {error}")),
        ),
        Ok(Err(error)) => (
            empty_pipe_output(),
            Some(format!(
                "failed to join subprocess {label} reader task: {error}"
            )),
        ),
        Err(_) => {
            task.abort();
            let _ = task.await;
            (
                empty_pipe_output(),
                Some(format!(
                    "subprocess {label} pipe did not close within {} ms",
                    PIPE_DRAIN_TIMEOUT.as_millis()
                )),
            )
        }
    }
}

#[cfg(windows)]
async fn run_cleanup_helper(
    mut command: Command,
    hide_window: bool,
    label: &str,
) -> Result<(), io::Error> {
    configure_process_group(&mut command, hide_window, false);
    command.kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to start {label} cleanup helper: {error}"),
        )
    })?;
    match timeout(CLEANUP_HELPER_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => cleanup_helper_status(status, label),
        Ok(Err(error)) => Err(io::Error::new(
            error.kind(),
            format!("failed to wait for {label} cleanup helper: {error}"),
        )),
        Err(_) => {
            let _ = child.start_kill();
            match timeout(CLEANUP_HELPER_REAP_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{label} cleanup helper exceeded {} ms and was terminated",
                        CLEANUP_HELPER_TIMEOUT.as_millis()
                    ),
                )),
                Ok(Err(error)) => Err(io::Error::new(
                    error.kind(),
                    format!("{label} cleanup helper timed out and could not be reaped: {error}"),
                )),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{label} cleanup helper timed out and did not exit within {} ms after termination",
                        CLEANUP_HELPER_REAP_TIMEOUT.as_millis()
                    ),
                )),
            }
        }
    }
}

#[cfg(windows)]
fn cleanup_helper_status(status: ExitStatus, label: &str) -> Result<(), io::Error> {
    if status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{label} cleanup helper exited unsuccessfully with {status}"
    )))
}

fn combine_cleanup_results(
    first: Result<(), io::Error>,
    second: Result<(), io::Error>,
) -> Result<(), io::Error> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(io::Error::other(format!("{first}; {second}"))),
    }
}

#[cfg(windows)]
struct WindowsJob {
    handle: isize,
}

#[cfg(windows)]
impl WindowsJob {
    fn assign(child: &Child) -> Result<Self, io::Error> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let process_handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("spawned process has no Windows process handle"))?
            as HANDLE;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        if unsafe { AssignProcessToJobObject(handle, process_handle) } == 0 {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        Ok(Self {
            handle: handle as isize,
        })
    }

    fn terminate(&self) -> Result<(), io::Error> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if unsafe { TerminateJobObject(self.handle as HANDLE, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn resume_windows_process(pid: u32) -> Result<(), io::Error> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let result = loop {
        if !has_entry {
            break Err(io::Error::other(format!(
                "suspended subprocess {pid} has no resumable primary thread"
            )));
        }
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                break Err(io::Error::last_os_error());
            }
            let resume_result = unsafe { ResumeThread(thread) };
            unsafe {
                CloseHandle(thread);
            }
            if resume_result == u32::MAX {
                break Err(io::Error::last_os_error());
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

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

        unsafe {
            CloseHandle(self.handle as HANDLE);
        }
    }
}

fn empty_pipe_output() -> BoundedPipeOutput {
    BoundedPipeOutput {
        bytes: Vec::new(),
        truncated: false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessTerminationStep {
    ParentStartKill,
    ProcessTreeKill,
    WaitForParent,
}

pub(crate) fn process_tree_termination_plan() -> Vec<ProcessTerminationStep> {
    vec![
        ProcessTerminationStep::ProcessTreeKill,
        ProcessTerminationStep::ParentStartKill,
        ProcessTerminationStep::WaitForParent,
    ]
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command, _hide_window: bool, _start_suspended: bool) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command, hide_window: bool, start_suspended: bool) {
    command.creation_flags(windows_creation_flags(hide_window, start_suspended));
}

#[cfg(windows)]
fn windows_creation_flags(hide_window: bool, start_suspended: bool) -> u32 {
    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    CREATE_NEW_PROCESS_GROUP
        | if hide_window { CREATE_NO_WINDOW } else { 0 }
        | if start_suspended { CREATE_SUSPENDED } else { 0 }
}

#[cfg(not(any(unix, windows)))]
fn configure_process_group(_command: &mut Command, _hide_window: bool, _start_suspended: bool) {}

#[cfg(unix)]
async fn kill_process_tree(pid: u32, _hide_window: bool) -> Result<(), io::Error> {
    let terminate_result = signal_unix_process_group(pid, libc::SIGTERM);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let kill_result = signal_unix_process_group(pid, libc::SIGKILL);
    combine_cleanup_results(terminate_result, kill_result)
}

#[cfg(unix)]
fn signal_unix_process_group(pid: u32, signal: i32) -> Result<(), io::Error> {
    let pid = i32::try_from(pid).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("process id {pid} does not fit the Unix pid type"),
        )
    })?;
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process id must be positive",
        ));
    }
    // SAFETY: a negative positive pid addresses exactly one process group and `signal` is one of
    // the platform signal constants supplied by the caller.
    if unsafe { libc::kill(-pid, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(windows)]
async fn kill_windows_process_tree(pid: u32, hide_window: bool) -> Result<(), io::Error> {
    let taskkill = windows_system_executable("taskkill.exe")?;
    let mut command = Command::new(taskkill);
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run_cleanup_helper(command, hide_window, "taskkill /T").await
}

#[cfg(windows)]
fn windows_system_executable(name: &str) -> Result<std::path::PathBuf, io::Error> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0u16; 260];
    loop {
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if (length as usize) < buffer.len() {
            buffer.truncate(length as usize);
            return Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer)).join(name));
        }
        buffer.resize(length as usize + 1, 0);
    }
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree(_pid: u32, _hide_window: bool) -> Result<(), io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::cleanup_helper_status;

    #[cfg(windows)]
    #[test]
    fn managed_child_is_suspended_but_cleanup_helper_is_not() {
        const CREATE_SUSPENDED: u32 = 0x0000_0004;

        assert_ne!(
            super::windows_creation_flags(true, true) & CREATE_SUSPENDED,
            0
        );
        assert_eq!(
            super::windows_creation_flags(true, false) & CREATE_SUSPENDED,
            0
        );
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_helper_rejects_nonzero_windows_exit_status() {
        use std::os::windows::process::ExitStatusExt;

        let status = std::process::ExitStatus::from_raw(7);
        assert!(cleanup_helper_status(status, "test").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn absent_process_group_cleanup_is_idempotent() {
        super::kill_process_tree(i32::MAX as u32, false)
            .await
            .expect("an already absent process group is clean");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn normal_process_exit_does_not_report_false_cleanup_failure() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut process = super::ManagedProcess::spawn(command, false, 1_024)
            .await
            .expect("spawn normal process");
        let status = process.wait().await.expect("wait normal process");

        let output = process.finish_after_exit(status).await;

        assert!(output.status.is_some_and(|status| status.success()));
        assert_eq!(output.cleanup_error(), None);
    }
}
