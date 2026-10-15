use super::*;
use crate::config::ResolvedConfig;
use crate::workspace::WorkspaceDiscovery;

fn fixture() -> (
    tempfile::TempDir,
    crate::tool::os_sandbox::SandboxPathSnapshot,
) {
    let parent = std::env::current_dir().unwrap().join("target");
    std::fs::create_dir_all(&parent).unwrap();
    let fixture = tempfile::Builder::new()
        .prefix("moyai-directory-watch-")
        .tempdir_in(parent)
        .unwrap();
    let root = Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).unwrap();
    let temp = Utf8PathBuf::from_path_buf(fixture.path().join("temp")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&temp).unwrap();
    let workspace =
        WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default()).unwrap();
    let profile = WorkspaceWriteSandboxProfile::compile(&workspace, [temp]).unwrap();
    let root = profile
        .writable_roots
        .into_iter()
        .find(|snapshot| snapshot.requested == root)
        .unwrap();
    (fixture, root)
}

fn settle(watch: &mut Watcher) -> ChangedRoot {
    let marker = watch.create_marker().unwrap();
    let result = watch.finish(Instant::now() + WATCH_FINISH_TIMEOUT).unwrap();
    drop(marker);
    result
}

#[test]
fn directory_watch_captures_immediate_creation_and_directory_rename_before_marker() {
    let (_fixture, root) = fixture();
    let mut watch = Watcher::start(root.clone()).unwrap();
    std::fs::write(root.requested.join("first.txt"), "created").unwrap();
    std::fs::rename(
        root.requested.join("first.txt"),
        root.requested.join("renamed.txt"),
    )
    .unwrap();
    std::fs::create_dir(root.requested.join("old-dir")).unwrap();
    std::fs::write(root.requested.join("old-dir/child.txt"), "child").unwrap();
    std::fs::rename(
        root.requested.join("old-dir"),
        root.requested.join("new-dir"),
    )
    .unwrap();
    let changed = settle(&mut watch);
    assert!(changed.paths.contains(&root.requested.join("first.txt")));
    assert!(changed.paths.contains(&root.requested.join("renamed.txt")));
    assert!(changed.paths.contains(&root.requested.join("new-dir")));
    assert!(!root.requested.join(&watch.marker).exists());
}

#[test]
fn directory_watch_noop_does_not_open_unchanged_unreadable_file() {
    let (_fixture, root) = fixture();
    let path = root.requested.join("unreadable.txt");
    std::fs::write(&path, "unchanged").unwrap();
    let handle = open_walk_object(&path, READ_CONTROL | WRITE_DAC).unwrap();
    let mut original = null_mut();
    let mut original_acl = null_mut();
    assert_eq!(
        unsafe {
            GetSecurityInfo(
                handle.raw(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut original_acl,
                null_mut(),
                &mut original,
            )
        },
        ERROR_SUCCESS
    );
    let _original = LocalAllocation(original);
    let mut descriptor = null_mut();
    assert_ne!(
        unsafe {
            windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(
        to_wide("D:P(A;;FA;;;SY)(A;;FA;;;BA)").as_ptr(), 1, &mut descriptor, null_mut())
        },
        0
    );
    let _descriptor = LocalAllocation(descriptor);
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = null_mut();
    assert_ne!(
        unsafe {
            windows_sys::Win32::Security::GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut acl,
                &mut defaulted,
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                handle.raw(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null_mut(),
            )
        },
        ERROR_SUCCESS
    );
    assert!(
        File::open(&path).is_err(),
        "test requires an ordinary non-admin caller"
    );
    let mut watch = Watcher::start(root).unwrap();
    let changed = settle(&mut watch);
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                handle.raw(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                original_acl,
                null_mut(),
            )
        },
        ERROR_SUCCESS
    );
    assert!(changed.paths.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
}

#[test]
#[ignore = "explicit large-tree regression creates 100001 pre-existing files"]
fn directory_watch_noop_does_not_traverse_a_tree_larger_than_handoff_limit() {
    let (_fixture, root) = fixture();
    let old = root.requested.join("old-tree");
    std::fs::create_dir(&old).unwrap();
    for index in 0..=MAX_HANDOFF_OBJECTS {
        File::create(old.join(format!("{index}.txt"))).unwrap();
    }
    let mut watch = Watcher::start(root).unwrap();
    assert!(settle(&mut watch).paths.is_empty());
}

#[test]
fn host_write_probe_preserves_delete_denial_of_identity_pin() {
    let (_fixture, root) = fixture();
    let path = root.requested.join("normal.txt");
    std::fs::write(&path, "unchanged").unwrap();
    let pin = open_walk_object(
        &path,
        READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
    )
    .unwrap();
    assert!(open_walk_object(&path, AclChange::AllowWrite.mask()).is_err());
    let host = open_walk_object(&path, AclChange::AllowWrite.mask() & !DELETE).unwrap();
    assert_eq!(
        opened_object_identity(pin.raw()).unwrap(),
        opened_object_identity(host.raw()).unwrap()
    );
}

fn record(action: u32, name: &str) -> Vec<u8> {
    let utf16: Vec<_> = name.encode_utf16().collect();
    let mut bytes = Vec::new();
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(action.to_le_bytes());
    bytes.extend((utf16.len() as u32 * 2).to_le_bytes());
    for value in utf16 {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

#[test]
fn directory_watch_overflow_and_malformed_records_cannot_succeed() {
    assert!(
        parse_notifications(&[])
            .unwrap_err()
            .to_string()
            .contains("overflow")
    );
    assert!(parse_notifications(&[0; 11]).is_err());
    assert!(parse_notifications(&record(999, "unknown")).is_err());
    for name in [
        "..\\outside",
        "C:\\outside",
        "\\outside",
        "folder\\..\\outside",
        "folder\\",
        ".",
    ] {
        assert!(
            parse_notifications(&record(FILE_ACTION_ADDED, name)).is_err(),
            "{name}"
        );
    }
    let mut bytes = record(FILE_ACTION_ADDED, "日本語 child.txt");
    let padded = (bytes.len() + 3) & !3;
    bytes.resize(padded, 0);
    bytes[..4].copy_from_slice(&(padded as u32).to_le_bytes());
    bytes.extend(record(FILE_ACTION_RENAMED_NEW_NAME, "renamed\\child.txt"));
    assert_eq!(
        parse_notifications(&bytes).unwrap(),
        vec![
            (FILE_ACTION_ADDED, "日本語 child.txt".into()),
            (FILE_ACTION_RENAMED_NEW_NAME, "renamed\\child.txt".into()),
        ]
    );
    bytes[..4].copy_from_slice(&4u32.to_le_bytes());
    assert!(parse_notifications(&bytes).is_err());
}
