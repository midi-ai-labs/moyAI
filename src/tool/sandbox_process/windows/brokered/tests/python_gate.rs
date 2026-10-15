use super::*;

fn python_request(
    profile: WorkspaceWriteSandboxProfile,
    root: &Utf8PathBuf,
    python: &Utf8PathBuf,
    site: &Utf8PathBuf,
    arguments: &[&str],
) -> SandboxedProcessOutput {
    let mut environment = crate::tool::sandbox_process::captured_process_environment(
        &ResolvedConfig::default().shell,
    );
    environment.insert("PYTHONPATH".into(), site.to_string());
    environment.insert("PYTHONNOUSERSITE".into(), "1".into());
    environment.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    environment.insert("PYTEST_DISABLE_PLUGIN_AUTOLOAD".into(), "1".into());
    let executable = ResolvedExecutable::resolve(python.as_str(), root, &environment)
        .expect("pin explicitly selected Python");
    let argv = std::iter::once(executable.path().to_string())
        .chain(arguments.iter().map(|value| (*value).to_string()))
        .collect();
    crate::sandbox_broker::execute(
        profile,
        SandboxedProcessRequest {
            executable,
            argv,
            cwd: root.clone(),
            environment,
            stdin: Vec::new(),
            timeout_ms: Some(60_000),
            max_output_bytes: 512 * 1024,
            hide_window: true,
            cancel: tokio_util::sync::CancellationToken::new(),
        },
        None,
    )
    .expect("broker Python execution")
}

#[test]
#[ignore = "requires approved broker testhost, MOYAI_TEST_PYTHON (uv python find --offline --system 3.13), and MOYAI_TEST_PYTEST_SITE"]
fn broker_python_pytest_private_temp_and_workspace() {
    let python = Utf8PathBuf::from_path_buf(
        std::env::var_os("MOYAI_TEST_PYTHON")
            .expect("explicit UV-resolved Python")
            .into(),
    )
    .expect("UTF-8 Python path");
    let site = Utf8PathBuf::from_path_buf(
        std::env::var_os("MOYAI_TEST_PYTEST_SITE")
            .expect("explicit existing pytest site directory")
            .into(),
    )
    .expect("UTF-8 pytest site path");
    assert!(python.is_absolute() && python.is_file());
    assert!(site.is_absolute() && site.join("pytest").is_dir() && site.join("_pytest").is_dir());
    let (_fixture, root, profile) = fixture();
    std::fs::write(
        root.join("test_private_temp.py"),
        r#"
import json
import pathlib
import sys
import tempfile
import pytest

def exercise(directory):
    directory = pathlib.Path(directory)
    path = directory / "child.txt"
    path.write_text("restricted", encoding="utf-8")
    assert path.read_text(encoding="utf-8") == "restricted"
    assert "child.txt" in {entry.name for entry in directory.iterdir()}

def test_private_temp_and_workspace(tmp_path):
    exercise(tmp_path)
    with tempfile.TemporaryDirectory() as directory:
        exercise(directory)
        with tempfile.TemporaryDirectory(dir=directory) as nested:
            exercise(nested)
    workspace = pathlib.Path("workspace-private")
    workspace.mkdir(mode=0o700)
    exercise(workspace)
    nested = workspace / "nested-private"
    nested.mkdir(mode=0o700)
    exercise(nested)
    normal = pathlib.Path("workspace-normal")
    normal.mkdir(mode=0o777)
    exercise(normal)
    pathlib.Path("runtime-versions.json").write_text(json.dumps({
        "python_executable": sys.executable, "python_version": sys.version,
        "pytest_version": pytest.__version__,
    }), encoding="utf-8")
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("conftest.py"),
        r#"
import sys
import pytest

def pytest_sessionfinish(session, exitstatus):
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    reporter.write_line("PINNED_PYTHON=" + sys.executable)
    reporter.write_line("PYTHON_VERSION=" + sys.version.replace("\n", " "))
    reporter.write_line("PYTEST_VERSION=" + pytest.__version__)
"#,
    )
    .unwrap();
    let output = python_request(
        profile.clone(),
        &root,
        &python,
        &site,
        &[
            "-B",
            "-X",
            "utf8",
            "-m",
            "pytest",
            "-p",
            "no:cacheprovider",
            "-q",
            "--tb=short",
            "test_private_temp.py",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout.bytes);
    assert_eq!(
        output.exit_code,
        Some(0),
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.effect_started && output.process_tree_reaped);
    assert!(
        output.cleanup_errors.is_empty(),
        "{:?}",
        output.cleanup_errors
    );
    assert!(
        stdout.contains("1 passed")
            && stdout.contains("PINNED_PYTHON=")
            && stdout.contains("PYTEST_VERSION="),
        "{stdout}"
    );
    let versions: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("runtime-versions.json")).unwrap())
            .unwrap();
    assert!(same_windows_path(
        camino::Utf8Path::new(versions["python_executable"].as_str().unwrap()),
        &python
    ));
    assert!(!versions["python_version"].as_str().unwrap().is_empty());
    assert!(!versions["pytest_version"].as_str().unwrap().is_empty());
    println!("{stdout}");
    println!("runtime_versions={versions}");
    for directory in [
        root.join("workspace-private"),
        root.join("workspace-private/nested-private"),
        root.join("workspace-normal"),
    ] {
        assert_eq!(
            std::fs::read_to_string(directory.join("child.txt")).unwrap(),
            "restricted"
        );
        std::fs::write(directory.join("host.txt"), "host").unwrap();
        assert!(std::fs::read_dir(&directory).unwrap().count() >= 2);
    }
    assert_eq!(
        std::fs::read_dir(
            profile
                .effect_temp_base
                .as_ref()
                .unwrap()
                .requested
                .as_std_path()
        )
        .unwrap()
        .count(),
        0
    );
    let script = r#"import pathlib
for directory in ['workspace-private','workspace-private/nested-private','workspace-normal']:
    root=pathlib.Path(directory)
    assert (root/'host.txt').read_text(encoding='utf-8') == 'host'
    (root/'next-effect.txt').write_text('next',encoding='utf-8')
    assert (root/'next-effect.txt').read_text(encoding='utf-8') == 'next'
print('next-restricted-effect-ok')
"#;
    let output = python_request(
        profile,
        &root,
        &python,
        &site,
        &["-B", "-X", "utf8", "-c", script],
    );
    assert_eq!(
        output.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.process_tree_reaped);
    assert!(
        output.cleanup_errors.is_empty(),
        "{:?}",
        output.cleanup_errors
    );
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("next-restricted-effect-ok"));
    for directory in [
        root.join("workspace-private"),
        root.join("workspace-private/nested-private"),
        root.join("workspace-normal"),
    ] {
        assert_eq!(
            std::fs::read_to_string(directory.join("next-effect.txt")).unwrap(),
            "next"
        );
    }
}
