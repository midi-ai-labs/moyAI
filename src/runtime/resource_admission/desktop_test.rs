//! Exercise the Desktop and dedicated Runner registry selectors in distinct processes.
use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROBE: &str = "runtime::resource_admission::desktop_test::desktop_test_resource_probe";

struct OwnedProbe {
    child: Child,
    log: Utf8PathBuf,
}
impl OwnedProbe {
    fn spawn(root: &Utf8Path, name: &str, mode: &str, expected: &str) -> Self {
        let log = root.join(format!("{name}.log"));
        let output = File::create(&log).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                PROBE,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MOYAI_DESKTOP_E2E_ROOT", root)
            .env("MOYAI_CONFIG_PATH", root.join("config/config.toml"))
            .env("MOYAI_DATA_DIR", root.join("data"))
            .env("MOYAI_DESKTOP_PREFS_PATH", root.join("prefs/desktop.toml"))
            .env("WEBVIEW2_USER_DATA_FOLDER", root.join("webview"))
            .env(
                "MOYAI_TEST_RESOURCE_REGISTRY",
                root.join("resource-admission"),
            )
            .env("MOYAI_RESOURCE_PROBE_MODE", mode)
            .env("MOYAI_RESOURCE_PROBE_EXPECTED", expected)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output);
        if mode == "invalid" {
            command.env("MOYAI_TEST_RESOURCE_REGISTRY", root.join("data"));
        }
        if mode == "missing_runner" {
            command.env_remove("MOYAI_DESKTOP_E2E_RUNNER");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        Self {
            child: command.spawn().unwrap(),
            log,
        }
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "{}\n{}",
                    self.log,
                    std::fs::read_to_string(&self.log).unwrap()
                );
                return;
            }
            assert!(Instant::now() < deadline, "Probe timeout: {}", self.log);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn wait_ready(&mut self, ready: &Utf8Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "Resource owner exited before readiness:\n{}",
                std::fs::read_to_string(&self.log).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "Resource owner readiness timeout"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for OwnedProbe {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn virtual_pc(parent: &Utf8Path, name: &str) -> Utf8PathBuf {
    let root = parent.join(name);
    for directory in [
        "config",
        "data",
        "prefs",
        "webview",
        "resource-admission",
        "workspace",
    ] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    root
}

#[test]
fn desktop_test_resource_exclusion_is_shared_with_runner_and_separate_between_instances() {
    let parent = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("project_sandbox/desktop-e2e-isolation-20260913/resource-focused");
    std::fs::create_dir_all(&parent).unwrap();
    let fixture = tempfile::tempdir_in(parent).unwrap();
    let fixture = Utf8Path::from_path(fixture.path()).unwrap();
    let a = virtual_pc(fixture, "pc-a");
    let b = virtual_pc(fixture, "pc-b");
    let mut owner = OwnedProbe::spawn(&a, "desktop-owner", "desktop", "hold");
    owner.wait_ready(&a.join("owner-ready"));
    OwnedProbe::spawn(&a, "same-pc-runner", "runner", "busy").finish();
    OwnedProbe::spawn(&b, "other-pc-desktop", "desktop", "free").finish();
    OwnedProbe::spawn(&b, "other-pc-runner", "runner", "free").finish();
    std::fs::write(a.join("owner-release"), b"release").unwrap();
    owner.finish();
    OwnedProbe::spawn(&a, "released-pc-runner", "runner", "free").finish();
    OwnedProbe::spawn(&a, "invalid-desktop-scope", "invalid", "invalid").finish();
    OwnedProbe::spawn(&a, "legacy-explicit-runner", "override", "free").finish();
    #[cfg(windows)]
    OwnedProbe::spawn(&a, "missing-test-runner", "missing_runner", "invalid").finish();
}

#[test]
#[ignore = "Dedicated child of desktop_test_resource_exclusion; requires isolated fixed-layout paths"]
fn desktop_test_resource_probe() {
    let root = Utf8PathBuf::from(std::env::var("MOYAI_DESKTOP_E2E_ROOT").unwrap())
        .canonicalize_utf8()
        .unwrap();
    assert!(root.is_absolute());
    let mode = std::env::var("MOYAI_RESOURCE_PROBE_MODE").unwrap();
    if mode == "missing_runner" {
        assert!(
            crate::runner::operations::launch().is_err(),
            "An isolated Desktop launched the normal machine Runner"
        );
        assert!(!root.join("data/runner.log").exists());
        return;
    }
    if mode == "invalid" {
        assert!(
            registry_directory().is_err(),
            "Invalid Desktop isolation fell back to a different registry"
        );
        return;
    }
    let expected_registry = root.join(if mode == "override" {
        "legacy-test-registry"
    } else {
        "resource-admission"
    });
    if matches!(mode.as_str(), "runner" | "override") {
        // Existing cfg(test) Runner launch uses an explicit registry before opening its host.
        set_test_registry(expected_registry.clone());
    }
    assert_eq!(registry_directory().unwrap(), expected_registry);
    let expected = std::env::var("MOYAI_RESOURCE_PROBE_EXPECTED").unwrap();
    let resource = ResourceGuard::acquire(&ResourceScope::Device, &root.join("workspace"));
    if expected == "busy" {
        assert!(
            resource.is_err(),
            "The same virtual PC granted a second Device lease"
        );
        return;
    }
    let _resource = resource.unwrap();
    if expected == "hold" {
        std::fs::write(root.join("owner-ready"), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("owner-release").exists() {
            assert!(Instant::now() < deadline, "Resource owner release timeout");
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        assert_eq!(expected, "free");
    }
}
