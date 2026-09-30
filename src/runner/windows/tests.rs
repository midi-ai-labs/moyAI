use super::*;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};

struct OwnedHost(Child);
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct ShutdownFixture {
    directory: tempfile::TempDir,
    config: camino::Utf8PathBuf,
    host: OwnedHost,
}

impl ShutdownFixture {
    fn start(exit_mode: Option<&str>) -> Self {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../project_sandbox/desktop-runner-shutdown");
        std::fs::create_dir_all(&base).unwrap();
        let directory = tempfile::tempdir_in(base).unwrap();
        let config =
            camino::Utf8PathBuf::from_path_buf(directory.path().join("config.toml")).unwrap();
        std::fs::write(&config, "[model]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"shutdown-fixture\"\nprovider_profile = \"openai_compatible\"\n").unwrap();
        let log = std::fs::File::create(directory.path().join("runner.log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            if exit_mode.is_some() {
                "runner::windows::tests::shutdown_delayed_process_fixture"
            } else {
                "runner::shared::process_fixture::isolated_runner_process"
            },
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        if let Some(mode) = exit_mode {
            command.env("MOYAI_TEST_SHUTDOWN_EXIT_MODE", mode);
        }
        let host = OwnedHost(
            command
                .env("MOYAI_CONFIG_PATH", &config)
                .env("MOYAI_DATA_DIR", directory.path().join("data"))
                .env(
                    "MOYAI_TEST_RESOURCE_REGISTRY",
                    directory.path().join("resources"),
                )
                .env_remove("MOYAI_TEST_SHARED_SETTINGS")
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let mut fixture = Self {
            directory,
            config,
            host,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(RunnerResponse::Identity { .. }) =
                request_for_config(&RunnerCommand::Identity, &fixture.config)
            {
                return fixture;
            }
            assert!(fixture.host.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "fixture readiness deadline");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[test]
fn desktop_shutdown_absent_runner_does_not_launch_or_create_profile() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/desktop-runner-shutdown");
    std::fs::create_dir_all(&base).unwrap();
    let directory = tempfile::tempdir_in(base).unwrap();
    let config = camino::Utf8PathBuf::from_path_buf(directory.path().join("unused.toml")).unwrap();
    assert!(!endpoint_present_for_config(&config).unwrap());
    shutdown_existing_for_config(&config).unwrap();
    assert!(!endpoint_present_for_config(&config).unwrap());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn desktop_shutdown_confirms_exact_process_and_leaves_other_profile_running() {
    let mut selected = ShutdownFixture::start(None);
    let mut other = ShutdownFixture::start(None);
    shutdown_existing_for_config(&selected.config).unwrap();
    assert!(selected.host.0.try_wait().unwrap().is_some());
    assert!(other.host.0.try_wait().unwrap().is_none());
    let RunnerResponse::Identity { identity } =
        request_for_config(&RunnerCommand::Identity, &other.config).unwrap()
    else {
        panic!("other profile identity");
    };
    assert_eq!(identity.process_id, other.host.0.id());
    assert!(identity.accepting);
    shutdown_existing_for_config(&selected.config).unwrap();
    shutdown_existing_for_config(&other.config).unwrap();
    assert!(other.host.0.try_wait().unwrap().is_some());
}

#[test]
fn desktop_shutdown_rejects_changed_process_and_stale_incarnation_before_effect() {
    let mut selected = ShutdownFixture::start(None);
    let mut other = ShutdownFixture::start(None);
    let connection = connect_for_config(&selected.config, None).unwrap();
    let RunnerResponse::Identity { identity } = connection
        .exchange(&RunnerCommand::Identity)
        .unwrap()
        .unwrap()
    else {
        panic!("selected identity");
    };
    let RunnerConnection { pipe, server } = connection;
    drop(pipe);
    assert!(shutdown_captured_for_config(&other.config, &server, identity.clone()).is_err());
    assert!(
        request_for_config(
            &RunnerCommand::Shutdown {
                runner_id: ulid::Ulid::new()
            },
            &selected.config
        )
        .is_err()
    );
    assert!(selected.host.0.try_wait().unwrap().is_none());
    assert!(other.host.0.try_wait().unwrap().is_none());
    let RunnerResponse::Identity { identity: current } =
        request_for_config(&RunnerCommand::Identity, &selected.config).unwrap()
    else {
        panic!("selected identity after stale shutdown");
    };
    assert_eq!(current.runner_id, identity.runner_id);
    assert!(current.accepting);
    shutdown_existing_for_config(&selected.config).unwrap();
    shutdown_existing_for_config(&other.config).unwrap();
}

#[test]
fn desktop_shutdown_waits_for_process_after_ack_and_lost_ack() {
    for mode in ["ack", "lost_ack"] {
        let mut fixture = ShutdownFixture::start(Some(mode));
        let started = Instant::now();
        shutdown_existing_for_config(&fixture.config).unwrap();
        assert!(fixture.host.0.try_wait().unwrap().is_some(), "{mode}");
        assert!(
            fixture.directory.path().join("data/pipe-closed").is_file(),
            "{mode}"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "{mode}: shutdown receipt must not substitute for process exit"
        );
    }
}

#[test]
fn desktop_shutdown_drains_dynamically_installed_shared_worker_without_hub() {
    let mut fixture = ShutdownFixture::start(Some("dynamic_shared"));
    shutdown_existing_for_config(&fixture.config).unwrap();
    assert!(fixture.host.0.try_wait().unwrap().unwrap().success());
    assert!(
        fixture
            .directory
            .path()
            .join("data/shared-worker-drained")
            .is_file()
    );
}

#[test]
#[ignore = "Dedicated process fixture: leave the OS process alive after IPC shutdown"]
fn shutdown_delayed_process_fixture() {
    let data = camino::Utf8PathBuf::from(std::env::var("MOYAI_DATA_DIR").unwrap());
    let registry =
        camino::Utf8PathBuf::from(std::env::var("MOYAI_TEST_RESOURCE_REGISTRY").unwrap());
    crate::runtime::resource_admission::set_test_registry(registry);
    let mode = std::env::var("MOYAI_TEST_SHUTDOWN_EXIT_MODE").unwrap();
    if mode == "dynamic_shared" {
        crate::runner::shared::process_fixture::prepare_endpoint_change_configuration();
    }
    let listener = LocalListener::bind().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let host = runtime.block_on(RunnerHost::open()).unwrap();
    if mode == "dynamic_shared" {
        let settings = host.installed_shared_settings().unwrap().unwrap();
        runtime
            .block_on(host.dispatch(RunnerCommand::Operations {
                runner_id: host.identity().runner_id,
                operation: crate::runner::operations::RunnerOperation::InstallSettings {
                    settings,
                    templates: vec![],
                },
            }))
            .unwrap();
        assert!(runtime.block_on(host.inner.shared_worker.lock()).is_some());
    }
    if mode == "ack" || mode == "dynamic_shared" {
        listener.serve(host.clone(), &runtime).unwrap();
    } else {
        assert_eq!(mode, "lost_ack");
        loop {
            let connected = unsafe { ConnectNamedPipe(listener.pipe.0, null_mut()) } != 0;
            let error = unsafe { GetLastError() };
            if connected || error != ERROR_PIPE_CONNECTED {
                if !connected && error == ERROR_NO_DATA {
                    unsafe {
                        DisconnectNamedPipe(listener.pipe.0);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            let deadline = Instant::now() + IO_DEADLINE;
            let bytes = read_frame(listener.pipe.0, deadline).unwrap();
            authenticate_client(listener.pipe.0, &listener.identity).unwrap();
            let command: RunnerCommand = serde_json::from_slice(&bytes).unwrap();
            let shutdown = matches!(command, RunnerCommand::Shutdown { .. });
            let response = runtime.block_on(host.dispatch(command));
            if shutdown {
                assert!(matches!(response, Ok(RunnerResponse::ShutdownRequested)));
                unsafe {
                    DisconnectNamedPipe(listener.pipe.0);
                }
                break;
            }
            write_frame(
                listener.pipe.0,
                &serde_json::to_vec(&response).unwrap(),
                deadline,
            )
            .unwrap();
            let _ = read_frame(listener.pipe.0, deadline);
            unsafe {
                DisconnectNamedPipe(listener.pipe.0);
            }
        }
        drop(listener);
    }
    runtime.block_on(host.wait_shutdown()).unwrap();
    if mode == "dynamic_shared" {
        assert!(runtime.block_on(host.inner.shared_worker.lock()).is_none());
        std::fs::write(data.join("shared-worker-drained"), "drained").unwrap();
    }
    std::fs::write(data.join("pipe-closed"), "closed").unwrap();
    // Simulates final worker/journal cleanup after the IPC endpoint is gone.
    std::thread::sleep(Duration::from_millis(500));
}

#[test]
fn concurrent_clients_keep_one_authenticated_runner() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/runner-ipc-contention");
    std::fs::create_dir_all(&base).unwrap();
    let directory = tempfile::tempdir_in(base).unwrap();
    let config = camino::Utf8PathBuf::from_path_buf(directory.path().join("config.toml")).unwrap();
    std::fs::write(&config, "[model]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"ipc-fixture\"\nprovider_profile = \"openai_compatible\"\n").unwrap();
    let log = std::fs::File::create(directory.path().join("runner.log")).unwrap();
    let mut host = OwnedHost(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runner::shared::process_fixture::isolated_runner_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MOYAI_CONFIG_PATH", &config)
            .env("MOYAI_DATA_DIR", directory.path().join("data"))
            .env(
                "MOYAI_TEST_RESOURCE_REGISTRY",
                directory.path().join("resources"),
            )
            .env_remove("MOYAI_TEST_SHARED_SETTINGS")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let identity = loop {
        if let Ok(RunnerResponse::Identity { identity }) =
            request_for_config(&RunnerCommand::Identity, &config)
        {
            break identity;
        }
        assert!(
            host.0.try_wait().unwrap().is_none(),
            "Runner exited before readiness"
        );
        assert!(Instant::now() < deadline, "Runner readiness deadline");
        std::thread::sleep(Duration::from_millis(25));
    };
    let start = std::sync::Barrier::new(16);
    let responses = std::thread::scope(|scope| {
        let clients: Vec<_> = (0..16)
            .map(|_| {
                scope.spawn(|| {
                    (0..16)
                        .map(|_| {
                            start.wait();
                            request_for_config(&RunnerCommand::Identity, &config)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        clients
            .into_iter()
            .flat_map(|client| client.join().unwrap())
            .collect::<Vec<_>>()
    });
    request_for_config(
        &RunnerCommand::Shutdown {
            runner_id: identity.runner_id,
        },
        &config,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "Runner shutdown deadline");
        std::thread::sleep(Duration::from_millis(25));
    }
    let failures: Vec<_> = responses
        .iter()
        .filter_map(|result| result.as_ref().err().map(|error| &error.message))
        .collect();
    assert!(
        failures.is_empty(),
        "{} concurrent Runner requests failed; first: {:?}",
        failures.len(),
        failures.first()
    );
    for response in responses {
        let RunnerResponse::Identity { identity: received } = response.unwrap() else {
            panic!("Unexpected Runner response");
        };
        assert_eq!(received.runner_id, identity.runner_id);
        assert_eq!(received.process_id, host.0.id());
    }
}
