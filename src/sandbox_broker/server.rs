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

mod lifecycle;

const STARTUP_GRACE: Duration = Duration::from_secs(10);
const MAX_FAMILIES: usize = 128;
const MAX_CONNECTIONS: usize = 32;
const MAX_ATTACHED_OWNERS: usize = MAX_CONNECTIONS / 2;

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

struct Service {
    families: Mutex<Families>,
    stop: CancellationToken,
    result: Mutex<Option<Result<(), String>>>,
}
static SERVICE: OnceLock<Service> = OnceLock::new();

struct Families {
    lifetime: lifecycle::Lifetime,
    entries: HashMap<String, Arc<Family>>,
}

struct Family {
    owners: Owners,
    stop: CancellationToken,
}

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
    service: &'a Service,
    family: Arc<Family>,
    caller: Arc<Peer>,
    id: String,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.service.release(&self.family, &self.id);
    }
}

impl Service {
    fn complete(&self, result: Result<(), String>, stopped: impl FnOnce(bool)) {
        let failed = result.is_err();
        if let Ok(mut saved) = self.result.lock() {
            *saved = Some(result);
        }
        // STOPPED can release the dispatcher immediately, so publish its result first.
        stopped(failed);
    }

    fn family(&self, session: &str, caller: Peer) -> Result<(Arc<Family>, Peer), String> {
        let mut families = self
            .families
            .lock()
            .map_err(|_| "Sandbox family registry is poisoned")?;
        if self.stop.is_cancelled() {
            return Err("Sandbox service is stopping".into());
        }
        if let Some(family) = families.entries.get(session) {
            native::authenticate_client(&caller, &family.owners.parent)?;
            if !family.owners.alive() {
                family.stop.cancel();
            }
            if family.stop.is_cancelled() {
                return Err("Sandbox application family has ended".into());
            }
            return Ok((family.clone(), caller));
        }
        if families.entries.len() >= MAX_FAMILIES {
            return Err("Sandbox service has reached its application-family limit".into());
        }
        let parent = Arc::new(Peer::open(caller.pid)?);
        if parent.created != caller.created {
            return Err("Sandbox application identity changed during registration".into());
        }
        native::authenticate_client(&caller, &parent)?;
        families.lifetime.admit(Instant::now())?;
        let family = Arc::new(Family {
            owners: Owners {
                parent: parent.clone(),
                leases: Mutex::new(HashMap::from([("launch".into(), parent)])),
            },
            stop: CancellationToken::new(),
        });
        families.entries.insert(session.into(), family.clone());
        Ok((family, caller))
    }

    fn check_family(&self, family: &Family) -> Result<(), String> {
        let _families = self
            .families
            .lock()
            .map_err(|_| "Sandbox family registry is poisoned")?;
        if !family.owners.alive() {
            family.stop.cancel();
        }
        if self.stop.is_cancelled() || family.stop.is_cancelled() {
            return Err("Sandbox application family is stopping".into());
        }
        Ok(())
    }

    fn attach<'a>(&'a self, family: &Arc<Family>, caller: Peer) -> Result<Lease<'a>, String> {
        let families = self
            .families
            .lock()
            .map_err(|_| "Sandbox family registry is poisoned")?;
        if !family.owners.alive() {
            family.stop.cancel();
        }
        if self.stop.is_cancelled() || family.stop.is_cancelled() {
            return Err("Sandbox application family is stopping".into());
        }
        let mut attached = 0;
        for existing in families.entries.values() {
            existing.owners.alive();
            attached += existing
                .owners
                .leases
                .lock()
                .map_err(|_| "Sandbox owner registry is poisoned")?
                .keys()
                .filter(|id| id.as_str() != "launch")
                .count();
        }
        if attached >= MAX_ATTACHED_OWNERS {
            return Err("Sandbox service has reached its shared application-attachment limit; existing owners were retained".into());
        }
        let id = ulid::Ulid::new().to_string();
        let caller = Arc::new(caller);
        family
            .owners
            .leases
            .lock()
            .map_err(|_| "Sandbox owner registry is poisoned")?
            .insert(id.clone(), caller.clone());
        Ok(Lease {
            service: self,
            family: family.clone(),
            caller,
            id,
        })
    }

    fn release(&self, family: &Family, id: &str) {
        let Ok(_families) = self.families.lock() else {
            self.stop.cancel();
            family.stop.cancel();
            return;
        };
        if let Ok(mut leases) = family.owners.leases.lock() {
            leases.remove(id);
        }
        if !family.owners.alive() {
            family.stop.cancel();
        }
    }

    fn shutdown_family(&self, family: &Family, caller: &Peer) -> Result<(), String> {
        let parent = &family.owners.parent;
        if caller.pid != parent.pid || caller.created != parent.created {
            return Err("Only the original application may release its launch lease".into());
        }
        self.release(family, "launch");
        Ok(())
    }

    fn stop(&self) {
        self.stop.cancel();
        if let Ok(mut families) = self.families.lock() {
            families.lifetime.stop();
            for family in families.entries.values() {
                family.stop.cancel();
            }
        }
    }

    fn begin_shutdown_if_idle(&self) -> Result<bool, String> {
        let mut families = self
            .families
            .lock()
            .map_err(|_| "Sandbox family registry is poisoned")?;
        let mut live_owners = false;
        for family in families.entries.values() {
            if !family.owners.alive() {
                family.stop.cancel();
            } else {
                live_owners = true;
            }
        }
        // A connection owns its family through execution, handback and response
        // acknowledgement. Owner death cancels work but cannot declare cleanup done.
        families
            .entries
            .retain(|_, family| !family.stop.is_cancelled() || Arc::strong_count(family) > 1);
        // Closing families remain while their workers own cleanup. Only the
        // absence of every live application owner begins the service's bounded drain.
        if families
            .lifetime
            .observe_empty(Instant::now(), !live_owners)
        {
            self.stop.cancel();
        }
        Ok(self.stop.is_cancelled())
    }
}

pub(super) fn run_service() -> Result<(), String> {
    let own_token = native::process_token(unsafe {
        windows_sys::Win32::System::Threading::GetCurrentProcess()
    })?;
    if super::token::user_sid(own_token.0)? != "S-1-5-18" {
        return Err("Sandbox service requires its approved SYSTEM launch".into());
    }
    let mut name = native::wide(super::SERVICE_NAME);
    SERVICE
        .set(Service {
            families: Mutex::new(Families {
                lifetime: lifecycle::Lifetime::new(Instant::now(), STARTUP_GRACE),
                entries: HashMap::new(),
            }),
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
    SERVICE
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
        if let Some(service) = SERVICE.get() {
            service.stop();
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
    let Some(launch) = SERVICE.get() else {
        return;
    };
    let name = native::wide(super::SERVICE_NAME);
    let service =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(service_control), null_mut()) };
    if service.is_null() {
        if let Ok(mut result) = launch.result.lock() {
            *result = Some(Err(native::last("register sandbox service control")));
        }
        return;
    }
    let result = std::panic::catch_unwind(|| {
        serve(
            launch,
            || status(service, 4, false),
            || status(service, 3, false),
        )
    })
    .unwrap_or_else(|_| Err("Sandbox service panicked; its process jobs are being closed".into()));
    launch.complete(result, |failed| status(service, 1, failed));
}

fn serve(
    launch: &'static Service,
    ready: impl FnOnce(),
    stopping: impl FnOnce(),
) -> Result<(), String> {
    // Eligible interactive users can identify the SYSTEM process and query its
    // token; they receive no token duplication or process mutation authority.
    native::allow_server_identity_query("IU")?;
    let name = super::service_pipe_name();
    // Retaining every instance throughout this service incarnation prevents replacement.
    let mut pipes = Vec::new();
    for index in 0..MAX_CONNECTIONS {
        pipes.push(native::create_pipe(name, "IU", index == 0)?);
    }
    let mut workers = Vec::new();
    for pipe in pipes {
        workers.push(std::thread::spawn(move || {
            while !launch.stop.is_cancelled() {
                if native::connect_server(pipe.0, || launch.stop.is_cancelled()).is_err() {
                    break;
                }
                if let Err(message) = connection(&pipe, launch) {
                    let _ = final_response(pipe.0, &Response::Error { message });
                }
                unsafe {
                    DisconnectNamedPipe(pipe.0);
                }
            }
        }));
    }
    ready();
    while !launch.begin_shutdown_if_idle()? {
        std::thread::sleep(Duration::from_millis(50));
    }
    stopping();
    launch.stop();
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

fn connection(pipe: &Handle, launch: &Service) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let session = match native::read_frame(pipe.0, || {
        if launch.stop.is_cancelled() {
            return Err("Sandbox service is stopping".into());
        }
        native::deadline_check(deadline)()
    })? {
        Request::Hello { version, session } if version == VERSION => session,
        _ => return Err("Sandbox handshake version or launch does not match".into()),
    };
    super::validate_session(&session)?;
    // Pipe impersonation is meaningful only after the first client message has been read.
    let caller = native::authenticate_peer(pipe.0)?;
    let (family, caller) = launch.family(&session, caller)?;
    let parent = &family.owners.parent;
    native::write_frame(
        pipe.0,
        &Response::Ready {
            version: VERSION,
            session,
            parent_pid: parent.pid,
            parent_created: parent.created,
        },
        native::deadline_check(deadline),
    )?;
    let request = native::read_frame(pipe.0, || {
        if family.stop.is_cancelled() || launch.stop.is_cancelled() {
            return Err("Sandbox application family is stopping".into());
        }
        native::deadline_check(deadline)()
    })?;
    match request {
        Request::Run { request } => {
            launch.check_family(&family)?;
            run(pipe, &caller, &family, *request)
        }
        Request::Attach {} => {
            let _lease = launch.attach(&family, caller)?;
            native::write_frame(
                pipe.0,
                &Response::Attached,
                native::deadline_check(deadline),
            )?;
            match native::read_frame::<Request>(pipe.0, || {
                if family.stop.is_cancelled() || !_lease.caller.live() {
                    Err("Sandbox application lease ended".into())
                } else {
                    Ok(())
                }
            })? {
                Request::Detach {} => Ok(()),
                _ => Err("Unexpected sandbox application lease message".into()),
            }
        }
        Request::Shutdown {} => {
            launch.shutdown_family(&family, &caller)?;
            native::write_frame(
                pipe.0,
                &Response::Stopping,
                native::deadline_check(deadline),
            )
        }
        _ => Err("Sandbox request is not valid for this connection".into()),
    }
}

fn run(pipe: &Handle, caller: &Peer, family: &Family, run: Run) -> Result<(), String> {
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
    let cancel = family.stop.child_token();
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
                if family.stop.is_cancelled() || !caller.live() {
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

    fn peer() -> Peer {
        Peer::open(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() }).unwrap()
    }

    fn service() -> Service {
        Service {
            families: Mutex::new(Families {
                lifetime: lifecycle::Lifetime::new(Instant::now(), STARTUP_GRACE),
                entries: HashMap::new(),
            }),
            stop: CancellationToken::new(),
            result: Mutex::new(None),
        }
    }

    #[test]
    fn stopped_notification_exposes_final_result_to_dispatcher() {
        for expected in [Ok(()), Err("service cleanup failed".to_owned())] {
            let service = service();
            service.complete(expected.clone(), |failed| {
                assert_eq!(failed, expected.is_err());
                // The dispatcher may return as soon as SCM observes STOPPED.
                assert_eq!(service.result.lock().unwrap().take(), Some(expected));
            });
        }
    }

    #[test]
    fn hello_retains_the_actual_parent_and_rejects_a_different_logon() {
        let service = service();
        let session = ulid::Ulid::new().to_string();
        let (family, caller) = service.family(&session, peer()).unwrap();
        assert_eq!(family.owners.parent.pid, caller.pid);
        assert_eq!(family.owners.parent.created, caller.created);
        let (same, _) = service.family(&session, peer()).unwrap();
        assert!(Arc::ptr_eq(&family, &same));
        for change in 0..3 {
            let mut foreign = peer();
            match change {
                0 => foreign.user.push_str("-other"),
                1 => foreign.session ^= 1,
                _ => foreign.authentication ^= 1,
            }
            assert!(service.family(&session, foreign).is_err());
        }
        assert!(!family.stop.is_cancelled());
        assert_eq!(service.families.lock().unwrap().entries.len(), 1);
    }

    #[test]
    fn shutdown_cancels_only_its_family_and_final_owner_begins_bounded_drain() {
        let service = service();
        let session = ulid::Ulid::new().to_string();
        let (first, caller) = service.family(&session, peer()).unwrap();
        let (second, _) = service
            .family(&ulid::Ulid::new().to_string(), peer())
            .unwrap();
        let mut reused_pid = peer();
        reused_pid.created ^= 1;
        assert!(service.shutdown_family(&first, &reused_pid).is_err());
        assert!(!first.stop.is_cancelled());
        service.shutdown_family(&first, &caller).unwrap();
        assert!(first.stop.is_cancelled());
        assert!(!second.stop.is_cancelled());
        assert!(service.family(&session, peer()).is_err());
        assert!(service.check_family(&first).is_err());
        assert!(service.check_family(&second).is_ok());
        assert!(!service.begin_shutdown_if_idle().unwrap());
        assert_eq!(service.families.lock().unwrap().entries.len(), 2);
        drop(first);
        assert!(!service.begin_shutdown_if_idle().unwrap());
        assert_eq!(service.families.lock().unwrap().entries.len(), 1);
        service.release(&second, "launch");
        assert!(service.begin_shutdown_if_idle().unwrap());
        drop(second);
        assert!(service.begin_shutdown_if_idle().unwrap());
        assert!(
            service
                .family(&ulid::Ulid::new().to_string(), peer())
                .is_err()
        );
    }

    #[test]
    fn attached_owner_keeps_work_alive_and_dropped_lease_cannot_leak() {
        let service = service();
        let (family, caller) = service
            .family(&ulid::Ulid::new().to_string(), peer())
            .unwrap();
        let lease = service.attach(&family, peer()).unwrap();
        service.shutdown_family(&family, &caller).unwrap();
        assert!(!family.stop.is_cancelled());
        assert!(!service.begin_shutdown_if_idle().unwrap());
        // This is also the rollback when writing Response::Attached fails.
        drop(lease);
        assert!(family.stop.is_cancelled());
        assert!(service.attach(&family, peer()).is_err());
        assert!(service.begin_shutdown_if_idle().unwrap());
        drop(family);
        assert!(service.begin_shutdown_if_idle().unwrap());
    }

    #[test]
    fn empty_owners_cannot_be_revived_before_the_idle_poll() {
        let service = service();
        let session = ulid::Ulid::new().to_string();
        let (family, _) = service.family(&session, peer()).unwrap();
        let pending_effect = family.stop.child_token();
        family.owners.leases.lock().unwrap().clear();
        assert!(!family.stop.is_cancelled());
        assert!(service.family(&session, peer()).is_err());
        assert!(pending_effect.is_cancelled());
        assert!(service.attach(&family, peer()).is_err());
        assert!(service.check_family(&family).is_err());
    }

    #[test]
    fn attachment_limit_preserves_existing_leases_and_execution_capacity() {
        let service = service();
        let (first, caller) = service
            .family(&ulid::Ulid::new().to_string(), peer())
            .unwrap();
        let (second, _) = service
            .family(&ulid::Ulid::new().to_string(), peer())
            .unwrap();
        let mut leases = Vec::new();
        for _ in 0..MAX_ATTACHED_OWNERS {
            leases.push(service.attach(&first, peer()).unwrap());
        }
        assert!(MAX_ATTACHED_OWNERS < MAX_CONNECTIONS);
        assert!(service.attach(&second, peer()).is_err());
        assert_eq!(first.owners.leases.lock().unwrap().len(), leases.len() + 1);
        assert!(service.check_family(&first).is_ok());
        assert!(service.check_family(&second).is_ok());
        service.shutdown_family(&first, &caller).unwrap();
        assert!(!first.stop.is_cancelled());
        drop(leases.pop());
        let replacement = service.attach(&second, peer()).unwrap();
        drop(leases);
        assert!(first.stop.is_cancelled());
        assert!(!second.stop.is_cancelled());
        drop(replacement);
    }

    #[test]
    fn service_stop_cancels_all_families_and_rejects_new_admission() {
        let service = service();
        let (family, _) = service
            .family(&ulid::Ulid::new().to_string(), peer())
            .unwrap();
        let pending_effect = family.stop.child_token();
        service.stop();
        assert!(pending_effect.is_cancelled());
        assert!(service.attach(&family, peer()).is_err());
        assert!(service.check_family(&family).is_err());
        assert!(
            service
                .family(&ulid::Ulid::new().to_string(), peer())
                .is_err()
        );
    }

    #[test]
    fn terminal_response_survives_until_the_client_consumes_it() {
        let peer =
            Peer::open(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() })
                .unwrap();
        let name = format!(
            r"\\.\pipe\moyai-terminal-response-test-{}",
            ulid::Ulid::new()
        );
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
