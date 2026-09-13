//! A real host process with compile-time-only isolation of machine resource policy.
//! Production binaries expose no policy override, environment switch, or alternate entry.

#[test]
#[ignore = "Dedicated child process: MOYAI_TEST_RESOURCE_REGISTRY and optional MOYAI_TEST_SHARED_SETTINGS"]
fn isolated_runner_process() {
    use super::{SharedSettings, SharedWorker};
    use crate::runner::{RunnerHost, RunnerResponse, windows};
    let directory = camino::Utf8PathBuf::from(
        std::env::var("MOYAI_TEST_RESOURCE_REGISTRY")
            .expect("Dedicated Runner fixture requires an explicit isolated registry"),
    );
    crate::runtime::resource_admission::set_test_registry(directory);
    let listener = windows::LocalListener::bind().expect("fixture listener");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let host = runtime.block_on(RunnerHost::open()).expect("fixture host");
    let settings = match std::env::var("MOYAI_TEST_SHARED_SETTINGS") {
        Ok(path) => {
            Some(SharedSettings::load(camino::Utf8Path::new(&path)).expect("fixture settings"))
        }
        Err(_) => host
            .installed_shared_settings()
            .expect("installed fixture settings"),
    };
    let shared = settings.map(|settings| {
        runtime
            .block_on(SharedWorker::start(host.clone(), settings))
            .expect("fixture shared worker")
    });
    println!(
        "{}",
        serde_json::to_string(&RunnerResponse::Identity {
            identity: host.identity()
        })
        .unwrap()
    );
    let result = listener.serve(host.clone(), &runtime);
    host.begin_shutdown().unwrap();
    runtime.block_on(host.wait_stopped());
    if let Some(shared) = shared {
        runtime.block_on(shared.wait()).unwrap();
    }
    result.unwrap();
}
