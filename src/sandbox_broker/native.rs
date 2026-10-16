use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use serde::{Serialize, de::DeserializeOwned};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

use super::{token, wire::MAX_FRAME};

pub(super) struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
impl Handle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
    pub fn checked(raw: HANDLE, stage: &str) -> Result<Self, String> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(last(stage))
        } else {
            Ok(Self(raw))
        }
    }
    pub fn duplicate(&self) -> Result<Self, String> {
        let mut value = null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.0,
                GetCurrentProcess(),
                &mut value,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(last("duplicate sandbox transport handle"));
        }
        Self::checked(value, "duplicate sandbox transport handle")
    }
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub(super) fn last(stage: &str) -> String {
    format!("{stage}: {}", io::Error::last_os_error())
}

pub(super) fn process_creation(process: HANDLE) -> Result<u64, String> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(last("query sandbox peer creation time"));
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

pub(super) fn process_token(process: HANDLE) -> Result<Handle, String> {
    let mut value = null_mut();
    if unsafe {
        OpenProcessToken(
            process,
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            &mut value,
        )
    } == 0
    {
        return Err(last("open sandbox peer token"));
    }
    Handle::checked(value, "open sandbox peer token")
}

pub(super) fn standard_token(value: HANDLE) -> Result<token::CallerIdentity, String> {
    if token::is_restricted(value)?
        || token::is_app_container(value)?
        || token::is_elevated(value)?
        || token::integrity_sid(value)? != "S-1-16-8192"
    {
        return Err("Sandbox helper accepts only an ordinary medium-integrity application".into());
    }
    token::caller_identity(value)
}

pub(super) struct Peer {
    pub process: Handle,
    pub token: Handle,
    pub pid: u32,
    pub created: u64,
    pub user: String,
    pub session: u32,
    pub authentication: u64,
}

impl Peer {
    pub fn open(pid: u32) -> Result<Self, String> {
        let process = Handle::checked(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) },
            "open sandbox peer",
        )?;
        if unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT {
            return Err("Sandbox peer has exited".into());
        }
        let created = process_creation(process.0)?;
        let token = process_token(process.0)?;
        let identity = standard_token(token.0)?;
        let authentication = token::authentication_id(token.0)?;
        Ok(Self {
            process,
            token,
            pid,
            created,
            user: identity.user_sid,
            session: identity.session_id,
            authentication,
        })
    }
    pub fn live(&self) -> bool {
        unsafe { WaitForSingleObject(self.process.0, 0) == WAIT_TIMEOUT }
    }
}

pub(crate) fn with_impersonation<T>(
    token: HANDLE,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if unsafe { ImpersonateLoggedOnUser(token) } == 0 {
        return Err(last("impersonate sandbox caller"));
    }
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            if unsafe { RevertToSelf() } == 0 {
                // A worker must never return to its pool with another identity still active.
                std::process::abort();
            }
        }
    }
    let revert = Revert;
    let result = operation();
    drop(revert);
    result
}

pub(super) fn authenticate_peer(pipe: HANDLE) -> Result<Peer, String> {
    let mut pid = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
        return Err(last("identify sandbox pipe client"));
    }
    let mut peer = Peer::open(pid)?;
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(last("authenticate sandbox pipe token"));
    }
    let result = (|| {
        let mut raw = null_mut();
        if unsafe {
            OpenThreadToken(
                GetCurrentThread(),
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
                1,
                &mut raw,
            )
        } == 0
        {
            return Err(last("query actual sandbox pipe token"));
        }
        let actual = Handle::checked(raw, "query actual sandbox pipe token")?;
        let identity = standard_token(actual.0)?;
        if identity.user_sid != peer.user
            || identity.session_id != peer.session
            || token::authentication_id(actual.0)? != peer.authentication
        {
            return Err("Sandbox pipe and process identities differ".into());
        }
        Ok(actual)
    })();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    peer.token = result?;
    Ok(peer)
}

pub(super) fn authenticate_client(peer: &Peer, launch: &Peer) -> Result<(), String> {
    if peer.user != launch.user
        || peer.session != launch.session
        || peer.authentication != launch.authentication
    {
        return Err("Sandbox pipe client belongs to a different Windows logon".into());
    }
    Ok(())
}

const PIPE_CLIENT_ACCESS: u32 = FILE_READ_DATA
    | FILE_WRITE_DATA
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | READ_CONTROL
    | SYNCHRONIZE;

pub(super) fn create_pipe(name: &str, user: &str, first: bool) -> Result<Handle, String> {
    // GENERIC_WRITE also grants FILE_CREATE_PIPE_INSTANCE for named pipes.
    // Clients may exchange frames, but only SYSTEM may add server instances.
    let descriptor = wide(&format!(
        "D:P(A;;GA;;;SY)(A;;0x{PIPE_CLIENT_ACCESS:x};;;{user})"
    ));
    let mut sd = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return Err(last("sandbox pipe ACL"));
    }
    let mut security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let value = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            32,
            65536,
            65536,
            0,
            &mut security,
        )
    };
    let result = Handle::checked(value, "create sandbox pipe");
    unsafe {
        LocalFree(sd);
    }
    result
}

pub(super) fn connect_server(pipe: HANDLE, stopped: impl Fn() -> bool) -> Result<(), String> {
    loop {
        if stopped() {
            return Err("Sandbox helper is stopping".into());
        }
        if unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0 {
            return Ok(());
        }
        match unsafe { GetLastError() } {
            ERROR_PIPE_CONNECTED => return Ok(()),
            ERROR_PIPE_LISTENING => std::thread::sleep(Duration::from_millis(10)),
            _ => return Err(last("accept sandbox client")),
        }
    }
}

pub(super) fn open_pipe(name: &str) -> Result<Handle, String> {
    let pipe = Handle::checked(
        unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                PIPE_CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
                null_mut(),
            )
        },
        "connect sandbox helper",
    )?;
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe { SetNamedPipeHandleState(pipe.0, &mode, null(), null()) } == 0 {
        return Err(last("set sandbox pipe wait mode"));
    }
    Ok(pipe)
}

fn transfer(
    handle: HANDLE,
    bytes: &mut [u8],
    writing: bool,
    mut keep_waiting: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < bytes.len() {
        keep_waiting()?;
        let mut count = 0;
        let length = (bytes.len() - offset).min(65536) as u32;
        let ok = unsafe {
            if writing {
                WriteFile(
                    handle,
                    bytes[offset..].as_ptr(),
                    length,
                    &mut count,
                    null_mut(),
                )
            } else {
                ReadFile(
                    handle,
                    bytes[offset..].as_mut_ptr(),
                    length,
                    &mut count,
                    null_mut(),
                )
            }
        };
        if ok == 0 {
            if unsafe { GetLastError() } != ERROR_NO_DATA {
                return Err(last("sandbox pipe transfer"));
            }
        } else if count > 0 {
            offset += count as usize;
            continue;
        } else if !writing {
            return Err("Sandbox pipe closed before the response completed".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

pub(super) fn write_frame<T: Serialize>(
    pipe: HANDLE,
    value: &T,
    mut keep_waiting: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("Sandbox message exceeds its size limit".into());
    }
    let mut length = (bytes.len() as u32).to_le_bytes();
    transfer(pipe, &mut length, true, &mut keep_waiting)?;
    transfer(pipe, &mut bytes, true, keep_waiting)
}

pub(super) fn read_frame<T: DeserializeOwned>(
    pipe: HANDLE,
    mut keep_waiting: impl FnMut() -> Result<(), String>,
) -> Result<T, String> {
    let mut length = [0u8; 4];
    transfer(pipe, &mut length, false, &mut keep_waiting)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("Sandbox message length is invalid".into());
    }
    let mut bytes = vec![0u8; length];
    transfer(pipe, &mut bytes, false, keep_waiting)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid sandbox message: {error}"))
}

pub(super) fn deadline_check(deadline: Instant) -> impl FnMut() -> Result<(), String> {
    move || {
        if Instant::now() < deadline {
            Ok(())
        } else {
            Err("Sandbox communication timed out".into())
        }
    }
}

pub(super) fn process_image(process: HANDLE) -> Result<std::path::PathBuf, String> {
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(last("query sandbox helper image"));
    }
    Ok(std::path::PathBuf::from(
        String::from_utf16(&buffer[..length as usize])
            .map_err(|_| "Sandbox helper path is not UTF-16")?,
    ))
}

pub(super) fn allow_server_identity_query(user: &str) -> Result<(), String> {
    fn apply(object: HANDLE, user: &str, rights: u32) -> Result<(), String> {
        let text = wide(&format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x{rights:x};;;{user})"
        ));
        let mut sd = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                null_mut(),
            )
        } == 0
        {
            return Err(last("sandbox identity descriptor"));
        }
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = null_mut();
            if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) }
                == 0
                || present == 0
                || dacl.is_null()
            {
                return Err(last("sandbox identity DACL"));
            }
            let status = unsafe {
                SetSecurityInfo(
                    object,
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null_mut(),
                )
            };
            if status != ERROR_SUCCESS {
                return Err(format!(
                    "Sandbox identity access: {}",
                    io::Error::from_raw_os_error(status as i32)
                ));
            }
            Ok(())
        })();
        unsafe {
            LocalFree(sd);
        }
        result
    }
    apply(
        unsafe { GetCurrentProcess() },
        user,
        PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
    )?;
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | WRITE_DAC, &mut raw) } == 0 {
        return Err(last("open sandbox server security"));
    }
    let handle = Handle::checked(raw, "open sandbox server security")?;
    apply(handle.0, user, TOKEN_QUERY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_clients_exchange_frames_without_creating_server_instances() {
        let peer = Peer::open(unsafe { GetCurrentProcessId() }).unwrap();
        let name = format!(r"\\.\pipe\moyai-pipe-rights-test-{}", ulid::Ulid::new());
        let server = create_pipe(&name, &peer.user, true).unwrap();
        let client = open_pipe(&name).unwrap();
        let extra = unsafe {
            CreateNamedPipeW(
                wide(&name).as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                32,
                65536,
                65536,
                0,
                null(),
            )
        };
        let error = unsafe { GetLastError() };
        if extra != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(extra) };
            panic!("ordinary pipe client created an additional server instance");
        }
        assert_eq!(error, ERROR_ACCESS_DENIED);
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            connect_server(server.raw(), || false).unwrap();
            let request: super::super::wire::Request =
                read_frame(server.raw(), deadline_check(deadline)).unwrap();
            assert!(matches!(
                request,
                super::super::wire::Request::Acknowledge {}
            ));
            write_frame(
                server.raw(),
                &super::super::wire::Response::Attached,
                deadline_check(deadline),
            )
            .unwrap();
            let _: super::super::wire::Request =
                read_frame(server.raw(), deadline_check(deadline)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        write_frame(
            client.raw(),
            &super::super::wire::Request::Acknowledge {},
            deadline_check(deadline),
        )
        .unwrap();
        let response: super::super::wire::Response =
            read_frame(client.raw(), deadline_check(deadline)).unwrap();
        assert!(matches!(response, super::super::wire::Response::Attached));
        write_frame(
            client.raw(),
            &super::super::wire::Request::Acknowledge {},
            deadline_check(deadline),
        )
        .unwrap();
        worker.join().unwrap();
    }

    fn restricted_token() -> Handle {
        let current = process_token(unsafe { GetCurrentProcess() }).unwrap();
        assert!(standard_token(current.0).is_ok());
        let mut raw = null_mut();
        let mut sid = null_mut();
        assert_ne!(
            unsafe { ConvertStringSidToSidW(wide("S-1-1-0").as_ptr(), &mut sid) },
            0
        );
        let restriction = SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: 0,
        };
        let created = unsafe {
            CreateRestrictedToken(
                current.0,
                1,
                0,
                null(),
                0,
                null(),
                1,
                &restriction,
                &mut raw,
            )
        };
        unsafe {
            LocalFree(sid);
        }
        assert_ne!(created, 0);
        Handle::checked(raw, "restricted test token").unwrap()
    }

    #[test]
    fn a_restricted_token_cannot_request_privileged_broker_operations() {
        let restricted = restricted_token();
        assert!(standard_token(restricted.0).is_err());
    }

    #[test]
    fn pipe_authentication_rejects_a_restricted_thread_inside_an_ordinary_process() {
        let restricted = restricted_token();
        let name = format!(r"\\.\pipe\moyai-pipe-token-test-{}", ulid::Ulid::new());
        // WORLD permits transport even under the fixture's restricting WORLD SID.
        // The broker must reject the actual token after reading the first frame.
        let server = create_pipe(&name, "WD", true).unwrap();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            connect_server(server.raw(), || Instant::now() >= deadline).unwrap();
            let _: super::super::wire::Request =
                read_frame(server.raw(), deadline_check(deadline)).unwrap();
            match authenticate_peer(server.raw()) {
                Ok(_) => panic!("restricted pipe token was treated as an ordinary application"),
                Err(message) => assert!(message.contains("ordinary medium-integrity")),
            }
        });
        let client = with_impersonation(restricted.raw(), || {
            let client = open_pipe(&name)?;
            write_frame(
                client.raw(),
                &super::super::wire::Request::Acknowledge {},
                deadline_check(Instant::now() + Duration::from_secs(10)),
            )?;
            Ok(client)
        })
        .unwrap();
        worker.join().unwrap();
        drop(client);
    }
}
