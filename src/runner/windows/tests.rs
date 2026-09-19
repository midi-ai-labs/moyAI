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
