use super::*;
fn store() -> (tempfile::TempDir, AuthStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = Utf8PathBuf::from_path_buf(dir.path().join("human.dpapi")).unwrap();
    (dir, AuthStore::new(path))
}
fn remembered(binding: &str, marker: char) -> Remembered {
    Remembered {
        binding: binding.into(),
        refresh_token: marker.to_string().repeat(64),
        token: "b".repeat(64),
        expires_at_ms: 100,
        principal: WorkPrincipal {
            user_id: "Alice".into(),
            display_name: "Alice".into(),
            administrator: false,
        },
    }
}

#[test]
fn remembered_auth_store_is_encrypted_and_restores_only_exact_binding() {
    let (_directory, store) = store();
    let record = remembered("Hub|CA|WinA", 'a');
    store.login(0, record.clone()).unwrap();
    let bytes = std::fs::read(&store.path).unwrap();
    for secret in [&record.refresh_token, &record.token, &record.binding] {
        assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }
    let reopened = AuthStore::new(store.path.clone()).load().unwrap();
    assert!(reopened.active.as_ref().is_some_and(|a| a == &record));
    assert!(
        !reopened
            .active
            .as_ref()
            .is_some_and(|a| a.binding == "Hub|CA|WinB")
    );
    assert!(
        !reopened
            .active
            .as_ref()
            .is_some_and(|a| a.binding == "Hub|OtherCA|WinA")
    );
    assert!(unprotect(b"not DPAPI data").is_err());
}

#[test]
fn remembered_auth_logout_survives_reopen_and_rejects_late_refresh_or_login() {
    let (_directory, store) = store();
    let old = remembered("Hub|CA|WinA", 'a');
    store.login(0, old.clone()).unwrap();
    let before_logout = store.load().unwrap().revision;
    store.logout().unwrap();
    let reopened = AuthStore::new(store.path.clone());
    let logged_out = reopened.load().unwrap();
    assert!(logged_out.active.is_none());
    assert_eq!(logged_out.pending_logouts.len(), 1);
    let session = LoginSession {
        token: "c".repeat(64),
        expires_at_ms: 1000,
        principal: old.principal.clone(),
        refresh_token: None,
    };
    assert!(reopened.update(&old, &session).is_err());
    assert!(
        reopened
            .login(before_logout, remembered("Hub|CA|WinA", 'd'))
            .is_err()
    );
    reopened
        .login(logged_out.revision, remembered("Hub|CA|WinA", 'd'))
        .unwrap();
    reopened.acknowledge_logout(&old).unwrap();
    let current = reopened.load().unwrap();
    assert_eq!(current.active.unwrap().refresh_token, "d".repeat(64));
    assert!(current.pending_logouts.is_empty());
}

#[test]
fn remembered_auth_corruption_is_not_replaced_and_parallel_writer_respects_lock() {
    let (_directory, store) = store();
    store.login(0, remembered("Hub|CA|WinA", 'a')).unwrap();
    let mut bytes = std::fs::read(&store.path).unwrap();
    let index = bytes.len() / 2;
    bytes[index] ^= 1;
    std::fs::write(&store.path, &bytes).unwrap();
    assert!(store.load().is_err());
    assert!(store.logout().is_err());
    assert_eq!(std::fs::read(&store.path).unwrap(), bytes);
    let (_directory, locked) = self::store();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(locked.path.with_extension("lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    assert!(locked.login(0, remembered("Hub|CA|WinA", 'a')).is_err());
    assert!(!locked.path.exists());
}
