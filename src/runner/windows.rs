//! Local named-pipe transport. Windows token evidence, not a caller-supplied user name or a
//! loopback bearer, binds both endpoints to the same logon and effective token attributes.

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

use super::{RunnerCommand, RunnerError, RunnerHost, RunnerResponse};

const MAX_FRAME: usize = 2 * 1024 * 1024;
const IO_DEADLINE: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    user: String,
    logon: (u32, i32),
    session: u32,
    integrity: String,
    groups: Vec<(String, u32)>,
    privileges: Vec<(u32, i32, u32)>,
}

fn last_error() -> RunnerError {
    RunnerError::new(io::Error::last_os_error().to_string())
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn token_buffer(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>, RunnerError> {
    let mut size = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut size);
    }
    if size == 0 || size > 256 * 1024 {
        return Err(last_error());
    }
    // Word alignment is required for TOKEN_* structures and their trailing arrays.
    let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    if unsafe { GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), size, &mut size) }
        == 0
    {
        return Err(last_error());
    }
    Ok(buffer)
}

fn sid_text(sid: PSID) -> Result<String, RunnerError> {
    let mut string = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut string) } == 0 {
        return Err(last_error());
    }
    let _allocation = LocalAllocation(string.cast());
    let mut length = 0;
    while unsafe { *string.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(string, length) })
        .map_err(|_| RunnerError::new("Windows SID could not be decoded"))
}

fn token_identity(token: HANDLE) -> Result<Identity, RunnerError> {
    let kind = token_buffer(token, TokenType)?;
    if unsafe { *kind.as_ptr().cast::<TOKEN_TYPE>() } == TokenImpersonation {
        let level = token_buffer(token, TokenImpersonationLevel)?;
        if unsafe { *level.as_ptr().cast::<SECURITY_IMPERSONATION_LEVEL>() } < SecurityImpersonation
        {
            return Err(RunnerError::new(
                "Runner requires a verifiable impersonation token",
            ));
        }
    }
    let elevation = token_buffer(token, TokenElevation)?;
    let app_container = token_buffer(token, TokenIsAppContainer)?;
    if unsafe { IsTokenRestricted(token) } != 0
        || unsafe { (*(elevation.as_ptr().cast::<TOKEN_ELEVATION>())).TokenIsElevated } != 0
        || unsafe { *app_container.as_ptr().cast::<u32>() } != 0
    {
        return Err(RunnerError::new(
            "Local Runner requires an unelevated, unrestricted user token; service, elevated, and sandboxed callers are unsupported",
        ));
    }
    let user = token_buffer(token, TokenUser)?;
    let statistics = token_buffer(token, TokenStatistics)?;
    let session = token_buffer(token, TokenSessionId)?;
    let integrity = token_buffer(token, TokenIntegrityLevel)?;
    let groups = token_buffer(token, TokenGroups)?;
    let privileges = token_buffer(token, TokenPrivileges)?;
    unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let statistics = &*statistics.as_ptr().cast::<TOKEN_STATISTICS>();
        let integrity = &*integrity.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let groups = &*groups.as_ptr().cast::<TOKEN_GROUPS>();
        let privileges = &*privileges.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let mut group_values =
            std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize)
                .iter()
                .map(|group| Ok((sid_text(group.Sid)?, group.Attributes)))
                .collect::<Result<Vec<_>, RunnerError>>()?;
        group_values.sort();
        let mut privilege_values = std::slice::from_raw_parts(
            privileges.Privileges.as_ptr(),
            privileges.PrivilegeCount as usize,
        )
        .iter()
        .map(|privilege| {
            (
                privilege.Luid.LowPart,
                privilege.Luid.HighPart,
                privilege.Attributes & !SE_PRIVILEGE_USED_FOR_ACCESS,
            )
        })
        .collect::<Vec<_>>();
        privilege_values.sort();
        let session = *session.as_ptr().cast::<u32>();
        if session == 0 {
            return Err(RunnerError::new(
                "A service-session Runner is not supported",
            ));
        }
        Ok(Identity {
            user: sid_text(user.User.Sid)?,
            logon: (
                statistics.AuthenticationId.LowPart,
                statistics.AuthenticationId.HighPart,
            ),
            session,
            integrity: sid_text(integrity.Label.Sid)?,
            groups: group_values,
            privileges: privilege_values,
        })
    }
}

fn process_identity(process: HANDLE) -> Result<Identity, RunnerError> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = Handle(token);
    token_identity(token.0)
}

fn current_identity() -> Result<Identity, RunnerError> {
    process_identity(unsafe { GetCurrentProcess() })
}

pub(crate) fn current_user_sid() -> Result<String, RunnerError> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = Handle(token);
    let user = token_buffer(token.0, TokenUser)?;
    sid_text(unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

fn pipe_name(identity: &Identity) -> String {
    let config_scope = crate::config::loader::global_config_path()
        .map(|path| path.to_string())
        .unwrap_or_default();
    pipe_name_in(identity, &config_scope)
}
fn pipe_name_in(identity: &Identity, config_scope: &str) -> String {
    let value = format!(
        "{}:{}:{}:{}:{}",
        identity.user,
        identity.logon.0,
        identity.logon.1,
        identity.session,
        config_scope.to_lowercase()
    );
    format!(
        r"\\.\pipe\moyai-runner-{:x}",
        Sha256::digest(value.as_bytes())
    )
}

pub fn endpoint_name() -> Result<String, RunnerError> {
    Ok(pipe_name(&current_identity()?))
}

/// Only an absent pipe permits automatic startup. Busy or inaccessible endpoints are
/// existing authority, never evidence to bypass identity verification with a new host.
pub(crate) fn endpoint_present() -> Result<bool, RunnerError> {
    let name = wide(&pipe_name(&current_identity()?));
    if unsafe { WaitNamedPipeW(name.as_ptr(), 0) } != 0 {
        return Ok(true);
    }
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND => Ok(false),
        ERROR_SEM_TIMEOUT | ERROR_PIPE_BUSY => Ok(true),
        _ => Err(last_error()),
    }
}

/// Bind before opening the store: FIRST_PIPE_INSTANCE prevents competing Runner startup recovery.
pub struct LocalListener {
    pipe: Handle,
    identity: Identity,
}

impl LocalListener {
    pub fn bind() -> Result<Self, RunnerError> {
        let identity = current_identity()?;
        let name = wide(&pipe_name(&identity));
        // Explicit protected DACL: only this user. Token comparison below additionally rejects
        // another logon session, reduced group/privilege rights, and mismatched integrity.
        let descriptor = wide(&format!("D:P(A;;GA;;;{})", identity.user));
        let mut descriptor_ptr = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor_ptr,
                null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        let _descriptor = LocalAllocation(descriptor_ptr);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_ptr,
            bInheritHandle: 0,
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(RunnerError::new(format!(
                "Cannot bind local Runner; it may already be running: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self {
            pipe: Handle(pipe),
            identity,
        })
    }

    /// One short request per connection. Disconnecting after acceptance never cancels its run.
    /// Nonblocking I/O plus a frame limit bounds unresponsive or malformed local clients.
    pub fn serve(
        self,
        host: RunnerHost,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<(), RunnerError> {
        while !host.is_stopped() {
            let connected = unsafe { ConnectNamedPipe(self.pipe.0, null_mut()) } != 0;
            let error = unsafe { GetLastError() };
            // In PIPE_NOWAIT mode a successful call only makes a disconnected instance
            // available to clients. ERROR_PIPE_CONNECTED is the actual connection evidence.
            if connected || error != ERROR_PIPE_CONNECTED {
                if connected {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                if error == ERROR_PIPE_LISTENING || error == ERROR_NO_DATA {
                    if error == ERROR_NO_DATA {
                        unsafe {
                            DisconnectNamedPipe(self.pipe.0);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(last_error());
            }
            let deadline = Instant::now() + IO_DEADLINE;
            // Read one bounded frame before impersonation, as required by Windows named pipes.
            // Do not deserialize or execute anything before verifying the actual client token.
            let result = read_frame(self.pipe.0, deadline).and_then(|bytes| {
                authenticate_client(self.pipe.0, &self.identity)?;
                let command = serde_json::from_slice::<RunnerCommand>(&bytes)
                    .map_err(|_| RunnerError::new("Invalid Runner request"))?;
                runtime.block_on(host.dispatch(command))
            });
            if let Ok(bytes) = serde_json::to_vec(&result) {
                if write_frame(self.pipe.0, &bytes, Instant::now() + IO_DEADLINE).is_ok() {
                    // DisconnectNamedPipe discards unread buffered data. The acknowledgement
                    // proves that the client consumed the response before disconnecting.
                    let _ = read_frame(self.pipe.0, Instant::now() + IO_DEADLINE);
                }
                // A receipt is not guaranteed delivered. Its run ID remains queryable if a
                // disconnect wins here, and retransmission within this incarnation is safe.
            }
            unsafe {
                DisconnectNamedPipe(self.pipe.0);
            }
        }
        Ok(())
    }
}

fn authenticate_client(pipe: HANDLE, expected: &Identity) -> Result<(), RunnerError> {
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(last_error());
    }
    let result = (|| {
        let mut token = null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            return Err(last_error());
        }
        let token = Handle(token);
        if token_identity(token.0)? != *expected {
            return Err(RunnerError::new(
                "Runner client must have the same Windows user, logon, and effective token authority",
            ));
        }
        Ok(())
    })();
    // Continuing to execute under an un-reverted impersonation token would be unsafe.
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    result
}

pub fn request(command: &RunnerCommand) -> Result<RunnerResponse, RunnerError> {
    let config = crate::config::loader::global_config_path()
        .map_err(|error| RunnerError::new(error.to_string()))?;
    request_for_config(command, &config)
}

pub(crate) fn request_for_config(
    command: &RunnerCommand,
    config: &camino::Utf8Path,
) -> Result<RunnerResponse, RunnerError> {
    let identity = current_identity()?;
    let name = wide(&pipe_name_in(&identity, config.as_str()));
    let connect_deadline = Instant::now() + Duration::from_secs(2);
    let pipe = loop {
        let remaining = connect_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero()
            || unsafe { WaitNamedPipeW(name.as_ptr(), remaining.as_millis().max(1) as u32) } == 0
        {
            return Err(RunnerError::new(
                "Local Runner is unavailable; start moyai-runner serve first",
            ));
        }
        let pipe = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
                null_mut(),
            )
        };
        if pipe != INVALID_HANDLE_VALUE {
            break Handle(pipe);
        }
        let error = io::Error::last_os_error();
        // Availability is not a reservation: another client can open the only
        // instance first. Retry only that pre-delivery race within the same bound.
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
            return Err(RunnerError::new(error.to_string()));
        }
    };
    let mut server_pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server_pid) } == 0 {
        return Err(last_error());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid) };
    if process.is_null() {
        return Err(last_error());
    }
    let process = Handle(process);
    if process_identity(process.0)? != identity {
        return Err(RunnerError::new(
            "Local Runner server identity does not match this Windows caller",
        ));
    }
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(pipe.0, &mode, null(), null()) } == 0 {
        return Err(last_error());
    }
    let bytes = serde_json::to_vec(command).map_err(|e| RunnerError::new(e.to_string()))?;
    let deadline = Instant::now() + IO_DEADLINE;
    write_frame(pipe.0, &bytes, deadline)?;
    let response = read_frame(pipe.0, deadline)?;
    let _ = write_frame(pipe.0, b"received", deadline);
    serde_json::from_slice::<Result<RunnerResponse, RunnerError>>(&response).map_err(|_| {
        RunnerError::new("Invalid Runner response; execution delivery may be uncertain")
    })?
}

fn read_exact(pipe: HANDLE, bytes: &mut [u8], deadline: Instant) -> Result<(), RunnerError> {
    let mut position = 0;
    while position < bytes.len() {
        let mut count = 0;
        let ok = unsafe {
            ReadFile(
                pipe,
                bytes[position..].as_mut_ptr(),
                (bytes.len() - position) as u32,
                &mut count,
                null_mut(),
            )
        } != 0;
        if ok && count > 0 {
            position += count as usize;
            continue;
        }
        let error = unsafe { GetLastError() };
        if !ok && error != ERROR_NO_DATA {
            return Err(last_error());
        }
        if Instant::now() >= deadline {
            return Err(RunnerError::new(
                "Runner pipe read timed out; delivery may be uncertain",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn read_frame(pipe: HANDLE, deadline: Instant) -> Result<Vec<u8>, RunnerError> {
    let mut length = [0u8; 4];
    read_exact(pipe, &mut length, deadline)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(RunnerError::new("Runner frame exceeds its limit"));
    }
    let mut bytes = vec![0u8; length];
    read_exact(pipe, &mut bytes, deadline)?;
    Ok(bytes)
}

fn write_frame(pipe: HANDLE, bytes: &[u8], deadline: Instant) -> Result<(), RunnerError> {
    if bytes.len() > MAX_FRAME {
        return Err(RunnerError::new("Runner response exceeds its limit"));
    }
    let frame = [(bytes.len() as u32).to_le_bytes().as_slice(), bytes].concat();
    let mut position = 0;
    while position < frame.len() {
        let mut count = 0;
        let ok = unsafe {
            WriteFile(
                pipe,
                frame[position..].as_ptr(),
                (frame.len() - position) as u32,
                &mut count,
                null_mut(),
            )
        } != 0;
        if !ok {
            return Err(last_error());
        }
        position += count as usize;
        if Instant::now() >= deadline {
            return Err(RunnerError::new(
                "Runner pipe write timed out; delivery may be uncertain",
            ));
        }
        if count == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}
