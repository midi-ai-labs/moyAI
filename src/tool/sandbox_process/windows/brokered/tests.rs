use super::*;
use crate::config::ResolvedConfig;
use crate::tool::executable::ResolvedExecutable;
use crate::workspace::WorkspaceDiscovery;

mod identity;
mod python_gate;

fn fixture() -> (tempfile::TempDir, Utf8PathBuf, WorkspaceWriteSandboxProfile) {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let fixture = tempfile::Builder::new()
        .prefix("moyai-broker-handoff-")
        .tempdir_in(parent)
        .unwrap();
    let root = Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).unwrap();
    let temp = Utf8PathBuf::from_path_buf(fixture.path().join("temp")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&temp).unwrap();
    let workspace =
        WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default()).unwrap();
    let profile = WorkspaceWriteSandboxProfile::compile(&workspace, [temp]).unwrap();
    (fixture, root, profile)
}

fn run(
    profile: WorkspaceWriteSandboxProfile,
    root: &Utf8PathBuf,
    script: String,
) -> SandboxedProcessOutput {
    let environment = crate::tool::sandbox_process::captured_process_environment(
        &ResolvedConfig::default().shell,
    );
    let executable = ResolvedExecutable::resolve("powershell.exe", root, &environment).unwrap();
    let request = SandboxedProcessRequest {
        argv: vec![
            executable.path().to_string(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script,
        ],
        executable,
        cwd: root.clone(),
        environment,
        stdin: Vec::new(),
        timeout_ms: Some(30_000),
        max_output_bytes: 128 * 1024,
        hide_window: true,
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    crate::sandbox_broker::execute(profile, request, None).unwrap()
}

fn literal(path: &camino::Utf8Path) -> String {
    path.as_str().replace('\'', "''")
}

const CREATE_PRIVATE: &str = r#"
$ErrorActionPreference='Stop';
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class PrivateDirectory {
 [StructLayout(LayoutKind.Sequential)] public struct SA { public int Length; public IntPtr Descriptor; public int Inherit; }
 [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string text,uint revision,out IntPtr descriptor,out uint bytes);
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateDirectoryW(string path,ref SA attributes);
 [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr allocation);
 public static void Create(string path) {
  IntPtr descriptor; uint bytes;
  if(!ConvertStringSecurityDescriptorToSecurityDescriptorW("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)",1,out descriptor,out bytes)) throw new System.ComponentModel.Win32Exception();
  try { var attributes=new SA { Length=Marshal.SizeOf(typeof(SA)),Descriptor=descriptor,Inherit=0 };
   if(!CreateDirectoryW(path,ref attributes)) throw new System.ComponentModel.Win32Exception();
  } finally { LocalFree(descriptor); }
 }
}
'@;
"#;

#[test]
fn broker_private_0700_temp_and_workspace_survive_host_and_next_effect() {
    let (_fixture, root, profile) = fixture();
    let private = root.join("private");
    let normal = root.join("normal");
    let script = format!(
        r#"{CREATE_PRIVATE}
[PrivateDirectory]::Create('{}');
New-Item -ItemType Directory -Path '{}' | Out-Null;
[PrivateDirectory]::Create((Join-Path $env:TEMP 'private'));
New-Item -ItemType Directory -Path (Join-Path $env:TEMP 'normal') | Out-Null;
foreach($directory in @('{}','{}',(Join-Path $env:TEMP 'private'),(Join-Path $env:TEMP 'normal'))) {{
 [IO.File]::WriteAllText((Join-Path $directory 'first.txt'),'first');
 if([IO.File]::ReadAllText((Join-Path $directory 'first.txt')) -ne 'first') {{ throw 'readback' }};
 if(@(Get-ChildItem -LiteralPath $directory).Count -ne 1) {{ throw 'enumeration' }};
}};
Write-Output private-and-normal-rw-ok;
"#,
        literal(&private),
        literal(&normal),
        literal(&private),
        literal(&normal)
    );
    let output = run(profile.clone(), &root, script);
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
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("private-and-normal-rw-ok"));
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
    for directory in [&private, &normal] {
        assert_eq!(
            std::fs::read_to_string(directory.join("first.txt")).unwrap(),
            "first"
        );
        std::fs::write(directory.join("host.txt"), "host").unwrap();
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 2);
    }
    let script = format!(
        r#"{CREATE_PRIVATE}
foreach($directory in @('{}','{}')) {{
 if([IO.File]::ReadAllText((Join-Path $directory 'host.txt')) -ne 'host') {{ throw 'host input' }};
 [PrivateDirectory]::Create((Join-Path $directory 'nested-private'));
 New-Item -ItemType Directory -Path (Join-Path $directory 'nested-normal') | Out-Null;
 foreach($nested in @('nested-private','nested-normal')) {{
  [IO.File]::WriteAllText((Join-Path (Join-Path $directory $nested) 'second.txt'),'second');
 }};
}};
Write-Output next-effect-rw-ok;
"#,
        literal(&private),
        literal(&normal)
    );
    let output = run(profile, &root, script);
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
    for directory in [&private, &normal] {
        for nested in ["nested-private", "nested-normal"] {
            let nested = directory.join(nested);
            assert_eq!(
                std::fs::read_to_string(nested.join("second.txt")).unwrap(),
                "second"
            );
            std::fs::write(nested.join("host-second.txt"), "host-second").unwrap();
        }
    }
}

#[test]
fn directory_handoff_inherits_to_future_children_without_changing_existing_child_acl() {
    let (_fixture, root, profile) = fixture();
    let directory = root.join("recovered");
    std::fs::create_dir(&directory).unwrap();
    let existing = directory.join("existing.txt");
    std::fs::write(&existing, "unchanged").unwrap();
    let before = dacl_bytes(&existing);
    let snapshot = profile
        .writable_roots
        .iter()
        .find(|snapshot| snapshot.requested == root)
        .unwrap();
    let capability = LocalSid::from_string(&capability_sid_for_root(snapshot)).unwrap();
    let token = open_current_process_token().unwrap();
    let host = LocalSid::from_string(&crate::sandbox_broker::token::user_sid(token.raw()).unwrap())
        .unwrap();
    let _root_pin = open_verified_identity_path(snapshot).unwrap();
    let pin = open_walk_object(
        &directory,
        READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES,
    )
    .unwrap();
    checked_walk_object(pin.raw(), &directory, &snapshot.canonical).unwrap();
    handoff_security(pin.raw(), &directory, host.raw(), capability.raw()).unwrap();
    assert_eq!(
        dacl_bytes(&existing),
        before,
        "unchecked existing child was changed by propagation"
    );
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "unchanged");
    for path in [
        directory.join("host.txt"),
        directory.join("host-dir/child.txt"),
    ] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "host input").unwrap();
        let child = open_walk_object(&path, READ_CONTROL | FILE_READ_ATTRIBUTES).unwrap();
        assert!(
            has_capability_write(child.raw(), capability.raw()).unwrap(),
            "future host child lost root capability: {path}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "host input");
    }
}

fn dacl_bytes(path: &camino::Utf8Path) -> Vec<u8> {
    let pin = open_walk_object(path, READ_CONTROL | FILE_READ_ATTRIBUTES).unwrap();
    let mut descriptor = null_mut();
    let mut acl = null_mut();
    assert_eq!(
        unsafe {
            GetSecurityInfo(
                pin.raw(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut acl,
                null_mut(),
                &mut descriptor,
            )
        },
        ERROR_SUCCESS
    );
    let _descriptor = LocalAllocation(descriptor);
    assert!(!acl.is_null());
    unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize).to_vec() }
}

#[test]
fn handoff_rejects_fresh_owner_hardlink_without_changing_shared_object() {
    let (_fixture, root, profile) = fixture();
    let script = "[IO.File]::WriteAllText('original.txt','content'); New-Item -ItemType HardLink -Path 'alias.txt' -Target 'original.txt' | Out-Null".to_string();
    let output = run(profile, &root, script);
    assert_eq!(
        output.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.process_tree_reaped);
    assert!(
        output
            .cleanup_errors
            .iter()
            .any(|error| error.contains("hard-linked")),
        "{:?}",
        output.cleanup_errors
    );
    assert_eq!(
        std::fs::read_to_string(root.join("original.txt")).unwrap(),
        "content"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("alias.txt")).unwrap(),
        "content"
    );
}

#[test]
fn handoff_skips_existing_junction_without_traversing_outside() {
    let (fixture, root, profile) = fixture();
    let outside = Utf8PathBuf::from_path_buf(fixture.path().join("outside")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("sentinel.txt"), "unchanged").unwrap();
    let command = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        literal(&root.join("existing-junction")),
        literal(&outside)
    );
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .status()
        .unwrap();
    assert!(status.success());
    let output = run(
        profile,
        &root,
        "[IO.File]::WriteAllText('normal.txt','content')".into(),
    );
    assert_eq!(output.exit_code, Some(0));
    assert!(
        output.cleanup_errors.is_empty(),
        "{:?}",
        output.cleanup_errors
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
        "unchanged"
    );
    // Remove the junction itself before TempDir cleanup; never recurse through it.
    std::fs::remove_dir(root.join("existing-junction")).unwrap();
}

#[test]
fn handoff_rejects_fresh_owner_junction_without_traversing_outside() {
    let (fixture, root, profile) = fixture();
    let outside = Utf8PathBuf::from_path_buf(fixture.path().join("outside")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("sentinel.txt"), "unchanged").unwrap();
    let script = format!(
        "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        literal(&root.join("fresh-junction")),
        literal(&outside)
    );
    let output = run(profile, &root, script);
    assert_eq!(
        output.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.process_tree_reaped);
    assert!(
        output
            .cleanup_errors
            .iter()
            .any(|error| error.contains("reparse")),
        "{:?}",
        output.cleanup_errors
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("sentinel.txt")).unwrap(),
        "unchanged"
    );
    std::fs::remove_dir(root.join("fresh-junction")).unwrap();
}
