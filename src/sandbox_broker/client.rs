use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandleEx,
    SYNCHRONIZE,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::*;

use super::installed::InstalledService;
use super::native::{self, Handle, Peer};
use super::wire::{Request, Response, Run, VERSION};
use crate::tool::os_sandbox::WorkspaceWriteSandboxProfile;
use crate::tool::sandbox_process::{
    ProcessStarted, SandboxExecutionError, SandboxedProcessOutput, SandboxedProcessRequest,
};

const INHERITED_ENDPOINT: &str = "MOYAI_SANDBOX_ENDPOINT";
static ACTIVE: OnceLock<Mutex<Option<Arc<Endpoint>>>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    session: String,
    parent_pid: u32,
    parent_created: u64,
    user: String,
}

pub struct LaunchGuard {
    endpoint: Arc<Endpoint>,
    owns_launch: bool,
    attached: Option<Handle>,
    _installed: InstalledService,
    _source_ancestors: Vec<File>,
    _source_pin: File,
    _runtime_pins: Vec<File>,
}

fn helper_path() -> Result<std::path::PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    #[cfg(test)]
    if current
        .parent()
        .is_some_and(|parent| parent.ends_with("deps"))
    {
        return Ok(current
            .parent()
            .and_then(|parent| parent.parent())
            .ok_or("Test binary has no target directory")?
            .join("moyai-sandbox.exe"));
    }
    Ok(current.with_file_name("moyai-sandbox.exe"))
}

fn verify_source_kind(file: &File, path: &Path, directory: bool) -> Result<(), String> {
    let mut tag = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            9,
            (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return Err(native::last("inspect sandbox startup source"));
    }
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "起動補助プログラムの配置にreparse pointがあります（{}）。通常のフォルダーへ配置してください。",
            path.display()
        ));
    }
    if (tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory {
        return Err(format!(
            "Sandbox startup source has the wrong object kind: {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn pin_source_ancestors(path: &Path) -> Result<Vec<File>, String> {
    if !path.is_absolute() {
        return Err("Sandbox startup source must have an absolute path".into());
    }
    let parent = path
        .parent()
        .ok_or("Sandbox startup source has no parent")?;
    let mut current = PathBuf::new();
    let mut pins = Vec::new();
    for component in parent.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err("Sandbox startup source contains a relative path component".into()),
        }
        // Retain each parent before resolving the next component. A target-file
        // handle alone does not prevent replacing a junction in its lexical path.
        // Attribute-only opens do not reserve ordinary sharing; LIST_DIRECTORY
        // makes the no-delete-sharing pin participate in the rename check.
        let pin = OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&current)
            .map_err(|error| {
                format!(
                    "pin sandbox startup directory {}: {error}",
                    current.display()
                )
            })?;
        verify_source_kind(&pin, &current, true)?;
        pins.push(pin);
    }
    Ok(pins)
}

pub(super) fn pin_helper(path: &Path) -> Result<(File, [u8; 32]), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| {
            format!(
                "起動補助プログラムを読み取れません（{}）: {error}",
                path.display()
            )
        })?;
    verify_source_kind(&file, path, false)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok((file, hash.finalize().into()))
}

pub(super) fn pin_bundled_runtime(path: &std::path::Path) -> Result<Vec<File>, String> {
    let directory = path
        .parent()
        .ok_or("Sandbox helper has no installation directory")?;
    let paths =
        ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"].map(|name| directory.join(name));
    let present = paths
        .iter()
        .map(|path| path.try_exists().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if present.iter().all(|present| !present) {
        return Ok(Vec::new());
    }
    if !present.iter().all(|present| *present) {
        return Err("同梱のWindows実行環境が不足しています。moyAIを再配置してください。".into());
    }
    paths
        .iter()
        .map(|path| pin_helper(path).map(|(file, _)| file))
        .collect()
}

fn active() -> Result<Arc<Endpoint>, String> {
    if let Some(endpoint) = ACTIVE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Sandbox launch state is poisoned")?
        .as_ref()
    {
        return Ok(endpoint.clone());
    }
    if let Some(encoded) = std::env::var_os(INHERITED_ENDPOINT) {
        let encoded = encoded
            .to_str()
            .ok_or("Sandbox launch identity is not UTF-8")?;
        if encoded.len() > 2048 {
            return Err("Sandbox launch identity exceeds its limit".into());
        }
        let endpoint: Endpoint =
            serde_json::from_str(encoded).map_err(|_| "Sandbox launch identity is invalid")?;
        super::validate_session(&endpoint.session)?;
        return Ok(Arc::new(endpoint));
    }
    Err("Windows実行補助サービスへの接続が確立していません。moyAIを再起動してください。コマンドを制限外で再実行することはありません。".into())
}

/// Passed explicitly only to an application-owned Runner, never to agent commands.
pub fn inherited_endpoint() -> Result<Option<String>, String> {
    let current = ACTIVE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Sandbox launch state is poisoned")?;
    current
        .as_ref()
        .map(|endpoint| serde_json::to_string(endpoint.as_ref()).map_err(|error| error.to_string()))
        .transpose()
}

fn connect(endpoint: &Endpoint, helper_hash: &[u8; 32]) -> Result<(Handle, Handle), String> {
    let installed = InstalledService::open(helper_hash)?;
    connect_installed(endpoint, helper_hash, &installed)
}

fn connect_installed(
    endpoint: &Endpoint,
    helper_hash: &[u8; 32],
    installed: &InstalledService,
) -> Result<(Handle, Handle), String> {
    let pid = installed.running_pid()?;
    connect_at(
        endpoint,
        helper_hash,
        super::service_pipe_name(),
        pid,
        |path| installed.is_image(path),
    )
}

fn connect_at(
    endpoint: &Endpoint,
    helper_hash: &[u8; 32],
    name: &str,
    expected_pid: u32,
    is_image: impl FnOnce(&Path) -> bool,
) -> Result<(Handle, Handle), String> {
    let pipe = native::open_pipe(name)?;
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe.0, &mut pid) } == 0 {
        return Err(native::last("identify sandbox server"));
    }
    if pid != expected_pid {
        return Err("Sandbox pipe server differs from the installed service process".into());
    }
    let server = Handle::checked(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) },
        "open sandbox server identity",
    )?;
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(server.0, TOKEN_QUERY, &mut raw) } == 0 {
        return Err(native::last("query sandbox server identity"));
    }
    let token = Handle::checked(raw, "query sandbox server identity")?;
    if super::token::user_sid(token.0)? != "S-1-5-18" {
        return Err("Sandbox server is not the approved SYSTEM helper".into());
    }
    let path = native::process_image(server.0)?;
    if !is_image(&path) {
        return Err("Sandbox server image differs from the fixed installed helper".into());
    }
    let (_pin, observed_hash) = pin_helper(&path)?;
    if &observed_hash != helper_hash {
        return Err("Sandbox helper binary differs from this moyAI installation".into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    native::write_frame(
        pipe.0,
        &Request::Hello {
            version: VERSION,
            session: endpoint.session.clone(),
        },
        native::deadline_check(deadline),
    )?;
    match native::read_frame(pipe.0, native::deadline_check(deadline))? {
        Response::Ready {
            version,
            session,
            parent_pid,
            parent_created,
        } if version == VERSION
            && session == endpoint.session
            && parent_pid == endpoint.parent_pid
            && parent_created == endpoint.parent_created =>
        {
            Ok((pipe, server))
        }
        Response::Error { message } => {
            acknowledge(pipe.0);
            Err(message)
        }
        _ => Err("Sandbox launch handshake did not match this application".into()),
    }
}

pub fn start() -> Result<LaunchGuard, String> {
    let peer = Peer::open(unsafe { GetCurrentProcessId() })?;
    let path = helper_path()?;
    let source_ancestors = pin_source_ancestors(&path)?;
    let (pin, hash) = pin_helper(&path)?;
    // Pin the existing app-local VC runtime set before connecting. Directory
    // pins prevent path replacement, not creation of previously absent files.
    let runtime_pins = pin_bundled_runtime(&path)?;
    let installed = InstalledService::open(&hash)?;
    if std::env::var_os(INHERITED_ENDPOINT).is_some() {
        let endpoint = active()?;
        if endpoint.user != peer.user {
            return Err("Inherited sandbox belongs to another Windows user".into());
        }
        let (pipe, _) = connect_installed(&endpoint, &hash, &installed)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        native::write_frame(
            pipe.0,
            &Request::Attach {},
            native::deadline_check(deadline),
        )?;
        if !matches!(
            native::read_frame::<Response>(pipe.0, native::deadline_check(deadline))?,
            Response::Attached
        ) {
            return Err("Sandbox application lease was not accepted".into());
        }
        *ACTIVE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "Sandbox launch state is poisoned")? = Some(endpoint.clone());
        return Ok(LaunchGuard {
            endpoint,
            owns_launch: false,
            attached: Some(pipe),
            _installed: installed,
            _source_ancestors: source_ancestors,
            _source_pin: pin,
            _runtime_pins: runtime_pins,
        });
    }
    if ACTIVE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Sandbox launch state is poisoned")?
        .is_some()
    {
        return Err("Sandbox startup must have a single application owner".into());
    }
    let endpoint = Arc::new(Endpoint {
        session: ulid::Ulid::new().to_string(),
        parent_pid: peer.pid,
        parent_created: peer.created,
        user: peer.user,
    });
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        // Only startup can retry an idle-stop race. Once this family is
        // established, losing the service never replays a command or a lease.
        match installed.start_if_stopped().and_then(|pid| {
            pid.ok_or_else(|| "Windows実行補助サービスを起動中です。".to_string())?;
            connect_installed(&endpoint, &hash, &installed)
        }) {
            Ok(_) => {
                *ACTIVE
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .map_err(|_| "Sandbox launch state is poisoned")? = Some(endpoint.clone());
                return Ok(LaunchGuard {
                    endpoint,
                    owns_launch: true,
                    attached: None,
                    _installed: installed,
                    _source_ancestors: source_ancestors,
                    _source_pin: pin,
                    _runtime_pins: runtime_pins,
                });
            }
            Err(error) => last_error = error,
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "起動補助機能の準備を確認できません。moyAIの起動を中止しました。{last_error}"
    ))
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        if let Ok(mut current) = ACTIVE.get_or_init(|| Mutex::new(None)).lock() {
            if current
                .as_ref()
                .is_some_and(|value| value.session == self.endpoint.session)
            {
                *current = None;
            }
        }
        if let Some(pipe) = self.attached.take() {
            let _ = native::write_frame(
                pipe.0,
                &Request::Detach {},
                native::deadline_check(Instant::now() + Duration::from_secs(1)),
            );
        }
        if self.owns_launch {
            if let Ok((_, hash)) = helper_path().and_then(|path| pin_helper(&path)) {
                if let Ok((pipe, _)) = connect(&self.endpoint, &hash) {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    let _ = native::write_frame(
                        pipe.0,
                        &Request::Shutdown {},
                        native::deadline_check(deadline),
                    );
                }
            }
            // A background Runner may retain its own lease beyond this Desktop's lifetime.
        }
    }
}

pub(crate) fn execute(
    profile: WorkspaceWriteSandboxProfile,
    request: SandboxedProcessRequest,
    started: Option<ProcessStarted>,
) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
    let error = SandboxExecutionError::Initialization;
    if request.cancel.is_cancelled() {
        return Ok(SandboxedProcessOutput {
            exit_code: None,
            stdout: crate::tool::truncate::BoundedPipeOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            stderr: crate::tool::truncate::BoundedPipeOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            timed_out: false,
            cancelled: true,
            effect_started: false,
            process_tree_reaped: true,
            cleanup_errors: Vec::new(),
        });
    }
    let endpoint = active().map_err(error)?;
    let (_pin, hash) = pin_helper(&helper_path().map_err(error)?).map_err(error)?;
    let (pipe, server) = connect(&endpoint, &hash).map_err(error)?;
    request
        .executable
        .revalidate()
        .map_err(|cause| error(cause.to_string()))?;
    let run = Run::from_request(profile, &request);
    run.validate().map_err(error)?;
    native::write_frame(
        pipe.0,
        &Request::Run {
            request: Box::new(run),
        },
        native::deadline_check(Instant::now() + Duration::from_secs(10)),
    )
    .map_err(error)?;
    let mut cancelled_sent = false;
    let mut cancel_deadline = None;
    loop {
        let response = native::read_frame(pipe.0, || {
            if unsafe { WaitForSingleObject(server.0, 0) } != WAIT_TIMEOUT {
                return Err(
                    "Sandbox helper exited before completion; cleanup is not confirmed".into(),
                );
            }
            if request.cancel.is_cancelled() && !cancelled_sent {
                native::write_frame(
                    pipe.0,
                    &Request::Cancel {},
                    native::deadline_check(Instant::now() + Duration::from_secs(3)),
                )?;
                cancelled_sent = true;
                cancel_deadline = Some(Instant::now() + Duration::from_secs(30));
            }
            if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err("Sandbox cancellation cleanup has not completed".into());
            }
            Ok(())
        })
        .map_err(SandboxExecutionError::Worker)?;
        match response {
            Response::Started { pid } => {
                if let Some(callback) = &started {
                    callback(pid);
                }
            }
            Response::Finished { result } => {
                acknowledge(pipe.0);
                return result;
            }
            Response::Error { message } => {
                acknowledge(pipe.0);
                return Err(SandboxExecutionError::Initialization(message));
            }
            _ => {
                return Err(SandboxExecutionError::Worker(
                    "Unexpected sandbox execution response".into(),
                ));
            }
        }
    }
}

fn acknowledge(pipe: windows_sys::Win32::Foundation::HANDLE) {
    let _ = native::write_frame(
        pipe,
        &Request::Acknowledge {},
        native::deadline_check(Instant::now() + Duration::from_secs(3)),
    );
}

#[cfg(test)]
mod tests {
    use sha2::Digest;

    fn publish_test_endpoint(
        path: &std::path::Path,
        mut contents: impl std::io::Read,
    ) -> std::io::Result<()> {
        let directory = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "endpoint has no parent")
        })?;
        let mut staged = tempfile::NamedTempFile::new_in(directory)?;
        std::io::copy(&mut contents, staged.as_file_mut())?;
        staged.as_file().sync_all()?;
        // Close the writer before publication so Windows readers can open immediately.
        staged
            .into_temp_path()
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
        Ok(())
    }

    #[test]
    fn endpoint_is_not_visible_until_all_bytes_are_written() {
        struct ObservedRead<'a> {
            endpoint: &'a std::path::Path,
            contents: &'a [u8],
        }
        impl std::io::Read for ObservedRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                assert!(!self.endpoint.try_exists()?);
                if self.contents.is_empty() {
                    return Ok(0);
                }
                buffer[0] = self.contents[0];
                self.contents = &self.contents[1..];
                Ok(1)
            }
        }
        let fixture = tempfile::tempdir().unwrap();
        let endpoint = fixture.path().join("endpoint.json");
        let contents = br#"{"ready":true}"#;
        publish_test_endpoint(
            &endpoint,
            ObservedRead {
                endpoint: &endpoint,
                contents,
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&endpoint).unwrap(), contents);
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
    }

    #[test]
    fn endpoint_publication_preserves_an_existing_file() {
        let fixture = tempfile::tempdir().unwrap();
        let endpoint = fixture.path().join("endpoint.json");
        std::fs::write(&endpoint, b"existing").unwrap();
        assert!(publish_test_endpoint(&endpoint, &b"replacement"[..]).is_err());
        assert_eq!(std::fs::read(&endpoint).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
    }

    #[test]
    fn endpoint_read_failure_does_not_publish_partial_bytes() {
        struct FailedRead;
        impl std::io::Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture read failure"))
            }
        }
        let fixture = tempfile::tempdir().unwrap();
        let endpoint = fixture.path().join("endpoint.json");
        assert!(publish_test_endpoint(&endpoint, FailedRead).is_err());
        assert!(!endpoint.try_exists().unwrap());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
    }

    fn source_fixture() -> tempfile::TempDir {
        let parent = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&parent).unwrap();
        tempfile::Builder::new()
            .prefix("moyai-startup-source-")
            .tempdir_in(parent)
            .unwrap()
    }

    #[test]
    fn ordinary_startup_source_and_adjacent_runtime_are_pinned_without_uac() {
        let fixture = source_fixture();
        let directory = fixture.path().join("ordinary");
        std::fs::create_dir(&directory).unwrap();
        let helper = directory.join("moyai-sandbox.exe");
        std::fs::write(&helper, b"benign helper fixture").unwrap();
        for name in ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"] {
            std::fs::write(directory.join(name), b"benign runtime fixture").unwrap();
        }
        let ancestors = super::pin_source_ancestors(&helper).unwrap();
        let (pin, hash) = super::pin_helper(&helper).unwrap();
        let runtime = super::pin_bundled_runtime(&helper).unwrap();
        assert_eq!(
            hash,
            <[u8; 32]>::from(super::Sha256::digest(b"benign helper fixture"))
        );
        assert_eq!(runtime.len(), 3);
        assert!(std::fs::rename(&directory, fixture.path().join("moved")).is_err());
        assert!(std::fs::write(&helper, b"replacement").is_err());
        assert!(std::fs::write(directory.join("vcruntime140.dll"), b"replacement").is_err());
        assert_eq!(std::fs::read(&helper).unwrap(), b"benign helper fixture");
        drop(runtime);
        drop(pin);
        assert!(std::fs::rename(&directory, fixture.path().join("moved")).is_err());
        drop(ancestors);
        std::fs::rename(&directory, fixture.path().join("moved")).unwrap();
    }

    #[test]
    fn preexisting_junction_source_ancestry_is_refused_before_connecting() {
        let fixture = source_fixture();
        let target = fixture.path().join("target");
        let link = fixture.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("moyai-sandbox.exe"), b"benign target").unwrap();
        let script = format!(
            "New-Item -ItemType Junction -Path '{}' -Target '{}' -ErrorAction Stop | Out-Null",
            link.to_string_lossy().replace('\'', "''"),
            target.to_string_lossy().replace('\'', "''")
        );
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .unwrap();
        assert!(status.success());
        let error = super::pin_source_ancestors(&link.join("moyai-sandbox.exe")).unwrap_err();
        assert!(error.contains("reparse point"), "{error}");
        assert_eq!(
            std::fs::read(target.join("moyai-sandbox.exe")).unwrap(),
            b"benign target"
        );
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn an_ordinary_process_cannot_impersonate_the_approved_server() {
        let peer = super::Peer::open(unsafe { super::GetCurrentProcessId() }).unwrap();
        let endpoint = super::Endpoint {
            session: ulid::Ulid::new().to_string(),
            parent_pid: peer.pid,
            parent_created: peer.created,
            user: peer.user,
        };
        let name = format!(r"\\.\pipe\moyai-fake-server-{}", endpoint.session);
        let _fake = super::native::create_pipe(&name, &endpoint.user, true).unwrap();
        let error = match super::connect_at(&endpoint, &[0; 32], &name, peer.pid, |_| true) {
            Ok(_) => panic!("ordinary process was trusted as SYSTEM"),
            Err(error) => error,
        };
        assert!(error.contains("not the approved SYSTEM helper"), "{error}");
    }

    /// An explicit test launch owns a family on the installed service for child tests.
    /// Its endpoint grants nothing without the actual ordinary Windows caller identity.
    #[test]
    #[ignore = "requires the installed sandbox service and MOYAI_SANDBOX_TEST_DIRECTORY"]
    fn launch_host() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("MOYAI_SANDBOX_TEST_DIRECTORY").expect("explicit test directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        let guard = super::start().expect("approved sandbox launch");
        publish_test_endpoint(
            &directory.join("endpoint.json"),
            &serde_json::to_vec(guard.endpoint.as_ref()).unwrap()[..],
        )
        .unwrap();
        while !directory.join("stop").exists() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        drop(guard);
    }
}
