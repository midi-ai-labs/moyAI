use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;

use super::native::{self, Handle, Peer};
use super::wire::{Request, Response, Run, VERSION};
use crate::tool::executable::ResolvedExecutable;
use crate::tool::sandbox_process::{SandboxExecutionError, SandboxedProcessRequest};

// These ABI declarations use the Windows service APIs without adding a runtime dependency.
#[repr(C)]
struct ServiceTableEntry {
    name: *mut u16,
    main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}
#[repr(C)]
struct ServiceStatus {
    service_type: u32,
    state: u32,
    accepted: u32,
    win32_exit: u32,
    service_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>,
        context: *mut c_void,
    ) -> *mut c_void;
    fn SetServiceStatus(handle: *mut c_void, status: *const ServiceStatus) -> i32;
}

struct Launch {
    session: String,
    parent_pid: u32,
    parent_created: u64,
    user: String,
    stop: CancellationToken,
    result: Mutex<Option<Result<(), String>>>,
}
static LAUNCH: OnceLock<Launch> = OnceLock::new();

struct Owners {
    parent: Arc<Peer>,
    leases: Mutex<HashMap<String, Arc<Peer>>>,
}
impl Owners {
    fn alive(&self) -> bool {
        let Ok(mut leases) = self.leases.lock() else {
            return false;
        };
        leases.retain(|_, peer| peer.live());
        !leases.is_empty()
    }
}

struct Lease<'a> {
    owners: &'a Owners,
    id: String,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Ok(mut leases) = self.owners.leases.lock() {
            leases.remove(&self.id);
        }
    }
}

pub(super) fn run_service(
    session: String,
    parent_pid: u32,
    parent_created: u64,
    user: String,
) -> Result<(), String> {
    let own_token = native::process_token(unsafe {
        windows_sys::Win32::System::Threading::GetCurrentProcess()
    })?;
    if super::token::user_sid(own_token.0)? != "S-1-5-18" {
        return Err("Sandbox service requires its approved SYSTEM launch".into());
    }
    let mut name = native::wide(&format!("moyAI-Sandbox-{session}"));
    LAUNCH
        .set(Launch {
            session,
            parent_pid,
            parent_created,
            user,
            stop: CancellationToken::new(),
            result: Mutex::new(None),
        })
        .map_err(|_| "Sandbox service has already started")?;
    let table = [
        ServiceTableEntry {
            name: name.as_mut_ptr(),
            main: Some(service_main),
        },
        ServiceTableEntry {
            name: null_mut(),
            main: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(native::last("start sandbox service dispatcher"));
    }
    LAUNCH
        .get()
        .unwrap()
        .result
        .lock()
        .map_err(|_| "Sandbox service result is poisoned")?
        .take()
        .unwrap_or_else(|| Err("Sandbox service did not enter its main function".into()))
}

unsafe extern "system" fn service_control(
    control: u32,
    _: u32,
    _: *mut c_void,
    _: *mut c_void,
) -> u32 {
    if matches!(control, 1 | 5) {
        if let Some(launch) = LAUNCH.get() {
            launch.stop.cancel();
        }
    }
    ERROR_SUCCESS
}

fn status(handle: *mut c_void, state: u32, failed: bool) {
    let status = ServiceStatus {
        service_type: 0x10,
        state,
        accepted: if state == 4 { 1 | 4 } else { 0 },
        win32_exit: if failed {
            ERROR_SERVICE_SPECIFIC_ERROR
        } else {
            0
        },
        service_exit: u32::from(failed),
        checkpoint: 0,
        wait_hint: if state == 3 { 30000 } else { 0 },
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Some(launch) = LAUNCH.get() else {
        return;
    };
    let name = native::wide(&format!("moyAI-Sandbox-{}", launch.session));
    let service =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(service_control), null_mut()) };
    if service.is_null() {
        if let Ok(mut result) = launch.result.lock() {
            *result = Some(Err(native::last("register sandbox service control")));
        }
        return;
    }
    let result = std::panic::catch_unwind(|| serve(launch, || status(service, 4, false)))
        .unwrap_or_else(|_| {
            Err("Sandbox service panicked; its process jobs are being closed".into())
        });
    status(service, 1, result.is_err());
    if let Ok(mut saved) = launch.result.lock() {
        *saved = Some(result);
    }
}

fn serve(launch: &'static Launch, ready: impl FnOnce()) -> Result<(), String> {
    let parent = Arc::new(Peer::open(launch.parent_pid)?);
    if !parent.matches_launch(launch.parent_created, &launch.user) {
        return Err("Sandbox launch parent identity changed".into());
    }
    native::allow_server_identity_query(&launch.user)?;
    let owners = Arc::new(Owners {
        parent: parent.clone(),
        leases: Mutex::new(HashMap::from([("launch".into(), parent)])),
    });
    let name = super::pipe_name(&launch.session);
    // Retaining every instance throughout the launch prevents replacement of the pipe name.
    let mut pipes = Vec::new();
    for index in 0..32 {
        pipes.push(native::create_pipe(&name, &launch.user, index == 0)?);
    }
    let mut workers = Vec::new();
    for pipe in pipes {
        let owners = owners.clone();
        workers.push(std::thread::spawn(move || {
            while !launch.stop.is_cancelled() && owners.alive() {
                if native::connect_server(pipe.0, || launch.stop.is_cancelled() || !owners.alive())
                    .is_err()
                {
                    break;
                }
                if let Err(message) = connection(&pipe, &owners, launch) {
                    let _ = final_response(pipe.0, &Response::Error { message });
                }
                unsafe {
                    DisconnectNamedPipe(pipe.0);
                }
            }
        }));
    }
    ready();
    while !launch.stop.is_cancelled() && owners.alive() {
        std::thread::sleep(Duration::from_millis(50));
    }
    launch.stop.cancel();
    let shutdown_deadline = Instant::now() + Duration::from_secs(30);
    while workers.iter().any(|worker| !worker.is_finished()) {
        if Instant::now() >= shutdown_deadline {
            // Returning stops this service process and closes all Job handles. Do not
            // report successful handback when a synchronous filesystem call is stuck.
            return Err("Sandbox shutdown exceeded its cleanup deadline; artifact handback is not confirmed".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for worker in workers {
        worker
            .join()
            .map_err(|_| "Sandbox connection worker panicked")?;
    }
    Ok(())
}

fn connection(pipe: &Handle, owners: &Owners, launch: &Launch) -> Result<(), String> {
    let parent = &owners.parent;
    let deadline = Instant::now() + Duration::from_secs(10);
    match native::read_frame(pipe.0, native::deadline_check(deadline))? {
        Request::Hello { version, session } if version == VERSION && session == launch.session => {
            ()
        }
        _ => return Err("Sandbox handshake version or launch does not match".into()),
    }
    // Pipe impersonation is meaningful only after the first client message has been read.
    let caller = native::authenticate_client(pipe.0, parent)?;
    native::write_frame(
        pipe.0,
        &Response::Ready {
            version: VERSION,
            session: launch.session.clone(),
            parent_pid: parent.pid,
            parent_created: parent.created,
        },
        native::deadline_check(deadline),
    )?;
    let request = native::read_frame(pipe.0, native::deadline_check(deadline))?;
    match request {
        Request::Run { request } => run(pipe, &caller, launch, *request),
        Request::Attach {} => {
            let id = ulid::Ulid::new().to_string();
            let caller = Arc::new(caller);
            owners
                .leases
                .lock()
                .map_err(|_| "Sandbox owner registry is poisoned")?
                .insert(id.clone(), caller.clone());
            let _lease = Lease { owners, id };
            native::write_frame(
                pipe.0,
                &Response::Attached,
                native::deadline_check(deadline),
            )?;
            match native::read_frame::<Request>(pipe.0, || {
                if launch.stop.is_cancelled() || !caller.live() {
                    Err("Sandbox application lease ended".into())
                } else {
                    Ok(())
                }
            })? {
                Request::Detach {} => Ok(()),
                _ => Err("Unexpected sandbox application lease message".into()),
            }
        }
        Request::Shutdown {} if caller.pid == parent.pid && caller.created == parent.created => {
            owners
                .leases
                .lock()
                .map_err(|_| "Sandbox owner registry is poisoned")?
                .remove("launch");
            native::write_frame(
                pipe.0,
                &Response::Stopping,
                native::deadline_check(deadline),
            )
        }
        _ => Err("Sandbox request is not valid for this connection".into()),
    }
}

fn run(pipe: &Handle, caller: &Peer, launch: &Launch, run: Run) -> Result<(), String> {
    run.validate()?;
    let executable = native::with_impersonation(caller.token.0, || {
        let executable =
            ResolvedExecutable::resolve(run.executable.as_str(), &run.cwd, &run.environment)
                .map_err(|error| error.to_string())?;
        if executable.path() != run.executable
            || executable.windows_identity() != &run.executable_identity
        {
            return Err("Admitted sandbox executable identity changed before execution".into());
        }
        Ok(executable)
    })?;
    let cancel = CancellationToken::new();
    let request = SandboxedProcessRequest {
        executable,
        argv: run.argv,
        cwd: run.cwd,
        environment: run.environment,
        stdin: run.stdin,
        timeout_ms: run.timeout_ms,
        max_output_bytes: run.max_output_bytes,
        hide_window: run.hide_window,
        cancel: cancel.clone(),
    };
    let control_pipe = pipe.duplicate()?;
    let write_pipe = Arc::new(Mutex::new(pipe.duplicate()?));
    let completed = CancellationToken::new();
    std::thread::scope(|scope| {
        let control = scope.spawn(|| {
            let request: Result<Request, String> = native::read_frame(control_pipe.raw(), || {
                if completed.is_cancelled() {
                    return Err("Sandbox effect completed".into());
                }
                if launch.stop.is_cancelled() || !caller.live() {
                    cancel.cancel();
                    return Err("Sandbox caller or launch has ended".into());
                }
                Ok(())
            });
            if !completed.is_cancelled() {
                // Cancellation, disconnect, and malformed follow-up all terminate the same Job.
                let _ = request;
                cancel.cancel();
            }
        });
        let started_pipe = write_pipe.clone();
        let started_cancel = cancel.clone();
        let started = Arc::new(move |pid| {
            let result = started_pipe
                .lock()
                .map_err(|_| "Sandbox response writer is poisoned".to_string())
                .and_then(|pipe| {
                    native::write_frame(
                        pipe.0,
                        &Response::Started { pid },
                        native::deadline_check(Instant::now() + Duration::from_secs(3)),
                    )
                });
            if result.is_err() {
                started_cancel.cancel();
            }
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::tool::sandbox_process::execute_brokered(
                run.profile,
                request,
                Some(started),
                caller.token.0,
            )
        }))
        .unwrap_or_else(|_| {
            Err(SandboxExecutionError::Worker(
                "Sandbox execution worker panicked".into(),
            ))
        });
        completed.cancel();
        control
            .join()
            .map_err(|_| "Sandbox cancellation reader panicked")?;
        let writer = write_pipe
            .lock()
            .map_err(|_| "Sandbox response writer is poisoned")?;
        final_response(writer.0, &Response::Finished { result })
    })
}

fn final_response(
    pipe: windows_sys::Win32::Foundation::HANDLE,
    response: &Response,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    native::write_frame(pipe, response, native::deadline_check(deadline))?;
    // DisconnectNamedPipe discards unread bytes. Keep this instance connected
    // until the caller has consumed the terminal response, without an unbounded flush.
    loop {
        match native::read_frame(pipe, native::deadline_check(deadline))? {
            Request::Acknowledge {} => return Ok(()),
            Request::Cancel {} => continue,
            _ => return Err("Unexpected terminal response acknowledgement".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_response_survives_until_the_client_consumes_it() {
        let peer =
            Peer::open(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() })
                .unwrap();
        let name = crate::sandbox_broker::pipe_name(&ulid::Ulid::new().to_string());
        let server = native::create_pipe(&name, &peer.user, true).unwrap();
        let client = native::open_pipe(&name).unwrap();
        let worker = std::thread::spawn(move || {
            native::connect_server(server.raw(), || false).unwrap();
            final_response(
                server.raw(),
                &Response::Error {
                    message: "x".repeat(128 * 1024),
                },
            )
            .unwrap();
            unsafe {
                DisconnectNamedPipe(server.raw());
            }
        });
        let response: Response = native::read_frame(
            client.0,
            native::deadline_check(Instant::now() + Duration::from_secs(10)),
        )
        .unwrap();
        assert!(
            matches!(response, Response::Error { message } if message == "x".repeat(128 * 1024))
        );
        assert!(
            !worker.is_finished(),
            "server disconnected before receipt acknowledgement"
        );
        native::write_frame(
            client.0,
            &Request::Acknowledge {},
            native::deadline_check(Instant::now() + Duration::from_secs(3)),
        )
        .unwrap();
        worker.join().unwrap();
    }
}
