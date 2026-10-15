use super::*;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{ERROR_IO_INCOMPLETE, ERROR_NOTIFY_ENUM_DIR};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OVERLAPPED,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_SECURITY,
    ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};

const BUFFER_BYTES: usize = 64 * 1024;
const MAX_CHANGED_PATHS: usize = 100_000;
const WATCH_FINISH_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct ChangedRoot {
    pub root: crate::tool::os_sandbox::SandboxPathSnapshot,
    pub paths: Vec<Utf8PathBuf>,
}

/// The observer owns read-only handles opened under the ordinary caller. It never
/// creates objects from its background thread, which has no caller impersonation.
pub(super) struct WatchSet {
    watches: Vec<Watcher>,
}

impl WatchSet {
    pub(super) fn start(
        profile: &WorkspaceWriteSandboxProfile,
        prepared: &PreparedSandbox,
    ) -> Result<Self, SandboxExecutionError> {
        let mut watches = Vec::new();
        for root in &prepared.roots {
            if profile.effect_temp_directory.as_ref() != Some(&root.root) {
                watches.push(Watcher::start(root.root.clone())?);
            }
        }
        Ok(Self { watches })
    }

    /// Called only after Job-zero. A marker in each watched stream settles the
    /// last read; cancelling an outstanding read is never a successful finish.
    pub(super) fn finish(
        mut self,
        caller: HANDLE,
    ) -> Result<Vec<ChangedRoot>, SandboxExecutionError> {
        let deadline = Instant::now() + WATCH_FINISH_TIMEOUT;
        let markers = impersonated(caller, || {
            self.watches
                .iter()
                .map(Watcher::create_marker)
                .collect::<Result<Vec<_>, _>>()
        })?;
        let result = (|| {
            let mut changes = Vec::new();
            for watch in &mut self.watches {
                changes.push(watch.finish(deadline)?);
            }
            Ok(changes)
        })();
        // DELETE_ON_CLOSE uses a handle admitted under the caller. Keep even its
        // close on that identity; no SYSTEM filesystem repair is performed.
        impersonated(caller, || {
            drop(markers);
            Ok(())
        })?;
        result
    }
}

impl Drop for WatchSet {
    fn drop(&mut self) {
        // Signal every stream before waiting for any individual worker.
        for watch in &self.watches {
            watch.abort.store(true, Ordering::Release);
        }
    }
}

struct Watcher {
    root: crate::tool::os_sandbox::SandboxPathSnapshot,
    marker: String,
    abort: Arc<AtomicBool>,
    result: Receiver<Result<BTreeSet<String>, SandboxExecutionError>>,
    worker: Option<JoinHandle<()>>,
}

impl Watcher {
    fn start(
        root: crate::tool::os_sandbox::SandboxPathSnapshot,
    ) -> Result<Self, SandboxExecutionError> {
        let directory = open_directory(&root)?;
        let marker = format!(".moyai-handoff-{}", ulid::Ulid::new());
        let abort = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let (result_tx, result) = mpsc::sync_channel(1);
        let worker_marker = marker.clone();
        let worker_abort = abort.clone();
        let worker = std::thread::Builder::new()
            .name("moyai-handoff-watch".into())
            .spawn(move || {
                let result = (|| {
                    let mut read = ActiveRead::new(directory)?;
                    read.begin()?;
                    if ready_tx.send(()).is_err() {
                        return Err(failure("directory watch startup was abandoned"));
                    }
                    observe(&mut read, &worker_marker, &worker_abort)
                })();
                let _ = result_tx.send(result);
            })
            .map_err(|error| failure(format!("start directory watcher: {error}")))?;
        let watch = Self {
            root,
            marker,
            abort,
            result,
            worker: Some(worker),
        };
        if ready.recv_timeout(WATCH_FINISH_TIMEOUT).is_err() {
            watch.abort.store(true, Ordering::Release);
            return Err(match watch.result.recv_timeout(CANCEL_SETTLE_TIMEOUT) {
                Ok(Err(error)) => error,
                _ => failure("directory watcher was not ready before process start"),
            });
        }
        Ok(watch)
    }

    fn create_marker(&self) -> Result<File, SandboxExecutionError> {
        let _pin = open_verified_identity_path(&self.root)?;
        OpenOptions::new()
            .write(true)
            .access_mode(GENERIC_WRITE_MASK | DELETE)
            .share_mode(FILE_SHARE_READ)
            .create_new(true)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(self.root.requested.join(&self.marker))
            .map_err(|error| failure(format!("create directory-watch end marker: {error}")))
    }

    fn finish(&mut self, deadline: Instant) -> Result<ChangedRoot, SandboxExecutionError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = self.result.recv_timeout(remaining).map_err(|_| {
            failure(
                "directory-watch end marker was not observed; handoff candidates are incomplete",
            )
        })?;
        let paths = result?;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| failure("directory watcher panicked"))?;
        }
        Ok(ChangedRoot {
            root: self.root.clone(),
            paths: paths
                .into_iter()
                .map(|relative| self.root.requested.join(relative))
                .collect(),
        })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // ActiveRead bounds cancellation settlement. A stuck kernel request
            // retains its resources instead of exposing freed OVERLAPPED memory.
            let deadline = Instant::now() + CANCEL_SETTLE_TIMEOUT + Duration::from_secs(1);
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

fn open_directory(
    root: &crate::tool::os_sandbox::SandboxPathSnapshot,
) -> Result<OwnedHandle, SandboxExecutionError> {
    let handle = OwnedHandle::new(
        unsafe {
            CreateFileW(
                to_wide(root.requested.as_str()).as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        },
        "open admitted directory watch",
    )?;
    verify_opened_path(handle.raw(), &root.canonical)?;
    if opened_object_identity(handle.raw())? != root.identity {
        return Err(failure("directory-watch root identity changed"));
    }
    Ok(handle)
}

struct ActiveRead {
    directory: OwnedHandle,
    event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    buffer: Vec<u32>,
    pending: bool,
}

impl ActiveRead {
    fn new(directory: OwnedHandle) -> Result<Self, SandboxExecutionError> {
        let event = OwnedHandle::new(
            unsafe { CreateEventW(null(), 1, 0, null()) },
            "directory-watch event",
        )?;
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { zeroed() });
        overlapped.hEvent = event.raw();
        Ok(Self {
            directory,
            event,
            overlapped,
            buffer: vec![0u32; BUFFER_BYTES / 4],
            pending: false,
        })
    }

    fn begin(&mut self) -> Result<(), SandboxExecutionError> {
        if unsafe { ResetEvent(self.event.raw()) } == 0 {
            return Err(win32("reset directory-watch event"));
        }
        if unsafe {
            ReadDirectoryChangesW(
                self.directory.raw(),
                self.buffer.as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_SECURITY,
                null_mut(),
                &mut *self.overlapped,
                None,
            )
        } == 0
        {
            return Err(win32("start directory watch"));
        }
        self.pending = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Vec<(u32, String)>>, SandboxExecutionError> {
        match unsafe { WaitForSingleObject(self.event.raw(), 20) } {
            WAIT_TIMEOUT => return Ok(None),
            WAIT_OBJECT_0 => (),
            _ => return Err(win32("wait directory watch")),
        }
        let mut count = 0;
        if unsafe { GetOverlappedResult(self.directory.raw(), &*self.overlapped, &mut count, 0) }
            == 0
        {
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_INCOMPLETE {
                self.pending = false;
            }
            return Err(failure(format!(
                "directory watch completion failed: Win32 {code}; candidates are incomplete"
            )));
        }
        self.pending = false;
        if count as usize > BUFFER_BYTES {
            return Err(failure("directory-watch result exceeds its buffer"));
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), count as usize)
        };
        parse_notifications(bytes).map(Some)
    }
}

impl Drop for ActiveRead {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        unsafe {
            CancelIoEx(self.directory.raw(), &*self.overlapped);
        }
        let deadline = Instant::now() + CANCEL_SETTLE_TIMEOUT;
        loop {
            let mut count = 0;
            if unsafe {
                GetOverlappedResult(self.directory.raw(), &*self.overlapped, &mut count, 0)
            } != 0
                || unsafe { GetLastError() } != ERROR_IO_INCOMPLETE
            {
                return;
            }
            if Instant::now() >= deadline {
                // CancelIoEx does not await completion. Leak only these operation
                // resources until process exit if the kernel refuses settlement.
                let resources = (
                    std::mem::replace(&mut self.directory, OwnedHandle(null_mut())),
                    std::mem::replace(&mut self.event, OwnedHandle(null_mut())),
                    std::mem::replace(&mut self.overlapped, Box::new(unsafe { zeroed() })),
                    std::mem::take(&mut self.buffer),
                );
                std::mem::forget(resources);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn observe(
    read: &mut ActiveRead,
    marker: &str,
    abort: &AtomicBool,
) -> Result<BTreeSet<String>, SandboxExecutionError> {
    let mut paths = BTreeSet::new();
    loop {
        if abort.load(Ordering::Acquire) {
            return Err(failure(
                "directory watch ended before its marker; candidates are incomplete",
            ));
        }
        let Some(records) = read.next()? else {
            continue;
        };
        let mut settled = false;
        for (action, name) in records {
            if action == FILE_ACTION_ADDED && name == marker {
                settled = true;
                continue;
            }
            paths.insert(name);
            if paths.len() > MAX_CHANGED_PATHS {
                return Err(failure("directory-watch changed-path limit exceeded"));
            }
        }
        // Process the entire completed buffer, including records following the
        // marker. There is no pending read to cancel on the success path.
        if settled {
            return Ok(paths);
        }
        read.begin()?;
    }
}

fn failure(message: impl Into<String>) -> SandboxExecutionError {
    SandboxExecutionError::Initialization(message.into())
}

fn parse_notifications(bytes: &[u8]) -> Result<Vec<(u32, String)>, SandboxExecutionError> {
    if bytes.is_empty() {
        return Err(failure(format!(
            "directory-watch overflow (Win32 {ERROR_NOTIFY_ENUM_DIR}); candidates are incomplete"
        )));
    }
    let mut offset = 0usize;
    let mut records = Vec::new();
    loop {
        let header = bytes
            .get(offset..offset + 12)
            .ok_or_else(|| failure("truncated directory-watch record"))?;
        let number = |start| u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
        let next = number(0) as usize;
        let action = number(4);
        let length = number(8) as usize;
        if length == 0
            || length % 2 != 0
            || !matches!(
                action,
                FILE_ACTION_ADDED
                    | FILE_ACTION_REMOVED
                    | FILE_ACTION_MODIFIED
                    | FILE_ACTION_RENAMED_OLD_NAME
                    | FILE_ACTION_RENAMED_NEW_NAME
            )
        {
            return Err(failure("invalid directory-watch action or filename"));
        }
        let name_bytes = bytes
            .get(offset + 12..offset + 12 + length)
            .ok_or_else(|| failure("truncated directory-watch filename"))?;
        let utf16: Vec<_> = name_bytes
            .chunks_exact(2)
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
            .collect();
        let name = String::from_utf16(&utf16)
            .map_err(|_| failure("directory-watch filename is not UTF-16"))?;
        if name.contains(['\0', ':', '/'])
            || name
                .split('\\')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
        {
            return Err(failure(
                "directory-watch filename is not a relative child path",
            ));
        }
        records.push((action, name));
        if next == 0 {
            break;
        }
        if next % 4 != 0 || next < 12 + length || next > bytes.len() - offset {
            return Err(failure("invalid directory-watch record offset"));
        }
        offset += next;
    }
    Ok(records)
}

#[cfg(test)]
mod tests;
