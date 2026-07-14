use std::io;
use std::process::{ExitStatus, Stdio};

use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::tool::truncate::{BoundedPipeOutput, read_pipe_bounded};

const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_HELPER_TIMEOUT: Duration = Duration::from_millis(1_500);
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
        configure_process_group(&mut command, hide_window);
        command.kill_on_drop(true);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn()?;
        #[cfg(windows)]
        let (job, job_error) = match WindowsJob::assign(&child) {
            Ok(job) => (Some(job), None),
            Err(error) => (None, Some(error)),
        };
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("spawned process has no process id"))?;
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
        if let Some(job_error) = job_error {
            let cleanup = process.terminate().await;
            let cleanup_suffix = cleanup
                .cleanup_error()
                .map(|error| format!("; fallback cleanup failed: {error}"))
                .unwrap_or_default();
            return Err(io::Error::other(format!(
                "failed to assign subprocess to a Windows Job Object: {job_error}{cleanup_suffix}"
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

async fn run_cleanup_helper(
    mut command: Command,
    hide_window: bool,
    label: &str,
) -> Result<(), io::Error> {
    configure_process_group(&mut command, hide_window);
    command.kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to start {label} cleanup helper: {error}"),
        )
    })?;
    match timeout(CLEANUP_HELPER_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
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
fn configure_process_group(command: &mut Command, _hide_window: bool) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command, hide_window: bool) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut flags = CREATE_NEW_PROCESS_GROUP;
    if hide_window {
        flags |= CREATE_NO_WINDOW;
    }
    command.creation_flags(flags);
}

#[cfg(not(any(unix, windows)))]
fn configure_process_group(_command: &mut Command, _hide_window: bool) {}

#[cfg(unix)]
async fn kill_process_tree(pid: u32, _hide_window: bool) -> Result<(), io::Error> {
    let process_group = format!("-{pid}");
    let mut terminate = Command::new("kill");
    terminate
        .args(["-TERM", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let terminate_result = run_cleanup_helper(terminate, false, "process-group TERM").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut kill = Command::new("kill");
    kill.args(["-KILL", &process_group])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let kill_result = run_cleanup_helper(kill, false, "process-group KILL").await;
    combine_cleanup_results(terminate_result, kill_result)
}

#[cfg(windows)]
async fn kill_windows_process_tree(pid: u32, hide_window: bool) -> Result<(), io::Error> {
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run_cleanup_helper(command, hide_window, "taskkill /T").await
}

#[cfg(not(any(unix, windows)))]
async fn kill_process_tree(_pid: u32, _hide_window: bool) -> Result<(), io::Error> {
    Ok(())
}
