//! Retire publication only after its existing execution owners prove quiescence.
use super::*;
use crate::runner::shared::RetirementJournalGuard;

pub(crate) struct RetirementGuard {
    _journals: Vec<RetirementJournalGuard>,
    #[cfg(windows)]
    _runner_startup: Vec<crate::runner::windows::LocalListener>,
    _user_data: Option<crate::storage::maintenance::UserDataQuiescence>,
    _registry: Option<Registry>,
    _publication: Option<File>,
}

impl RetirementGuard {
    fn empty() -> Self {
        Self {
            _publication: None,
            _registry: None,
            _journals: vec![],
            #[cfg(windows)]
            _runner_startup: vec![],
            _user_data: None,
        }
    }
}

pub(super) fn validate_registration_identity(
    config: &Utf8Path,
    settings: &SharedSettings,
) -> Result<(), RunnerError> {
    let directory = config.with_file_name("device-network");
    if crate::device_network::reset::execution_identity_reset(
        &directory,
        &settings.hub_id,
        &settings.device_id,
    )
    .map_err(error)?
    {
        return Err(error(
            "The shared Hub/device identity has been retired locally",
        ));
    }
    // Cleanup also removes the retirement document. Re-read the existing
    // credential owner before publication so a pre-cleanup startup cannot
    // republish its cached authority after the deletion guard is released.
    let device = crate::device_network::DeviceSettingsStore::new(directory.join("device.json"))
        .load()
        .map_err(error)?;
    if device.hub_id.as_deref() != Some(&settings.hub_id)
        || device.device_id.as_deref() != Some(&settings.device_id)
    {
        return Err(error(
            "Shared publication no longer matches the enrolled Hub/device identity",
        ));
    }
    Ok(())
}

pub(super) fn validate_replacement_identity(
    config: &Utf8Path,
    settings: &SharedSettings,
    entries: &[PublishedRoot],
) -> Result<(), RunnerError> {
    if entries.iter().any(|entry| {
        entry.config_path == config
            && (entry.hub_id != settings.hub_id || entry.device_id != settings.device_id)
    }) {
        return Err(error(
            "A different Hub/device is still published by this profile; retire its settled execution resources before replacing it",
        ));
    }
    Ok(())
}

pub(crate) fn reset_publication_pending(config: &Utf8Path) -> Result<bool, RunnerError> {
    reset_publication_pending_at(&registry_directory()?, config)
}

fn reset_publication_pending_at(
    directory: &Utf8Path,
    config: &Utf8Path,
) -> Result<bool, RunnerError> {
    let registry = Registry::open_at(directory, false)?;
    let retired =
        crate::device_network::reset::ResetState::load(&config.with_file_name("device-network"))
            .map_err(error)?;
    Ok(registry.entries.iter().any(|entry| {
        entry.config_path == config && retired.retired(&entry.hub_id, &entry.device_id)
    }))
}

pub(crate) fn retire_reset_resources() -> Result<RetirementGuard, RunnerError> {
    let config = crate::config::loader::global_config_path().map_err(error)?;
    let data = crate::storage::StoragePaths::discover()
        .map_err(error)?
        .data_dir;
    retire_reset_resources_for_profile(&config, &data)
}

pub(crate) fn retire_reset_resources_for_profile(
    config: &Utf8Path,
    data: &Utf8Path,
) -> Result<RetirementGuard, RunnerError> {
    let directory = registry_directory()?;
    retire_reset_resources_at(&directory, config, data, &operator_sid()?)
}

fn retire_reset_resources_at(
    directory: &Utf8Path,
    config: &Utf8Path,
    data: &Utf8Path,
    sid: &str,
) -> Result<RetirementGuard, RunnerError> {
    if !reset_publication_pending_at(directory, config)? {
        return Ok(RetirementGuard::empty());
    }
    validate_profile_data(data)?;
    retire_at(directory, config, data, sid, None)
}

pub(crate) fn prepare_user_data_reset(
    targets: &[Utf8PathBuf],
) -> Result<RetirementGuard, RunnerError> {
    if targets.is_empty() || targets.iter().any(|path| !path.is_absolute()) {
        return Err(error(
            "User-data reset requires explicit absolute target directories",
        ));
    }
    let config = crate::config::loader::global_config_path().map_err(error)?;
    let data = crate::storage::StoragePaths::discover()
        .map_err(error)?
        .data_dir;
    if !targets
        .iter()
        .any(|target| within(&config, target).unwrap_or(false))
        || !targets
            .iter()
            .any(|target| within(&data, target).unwrap_or(false))
    {
        return Err(error(
            "The active config/data profile is outside the approved cleanup targets",
        ));
    }
    validate_profile_data(&data)?;
    let directory = registry_directory()?;
    let registry = Registry::open_at(&directory, false)?;
    for entry in &registry.entries {
        if targets
            .iter()
            .any(|target| within(&entry.config_path, target).unwrap_or(false))
            && entry.config_path != config
        {
            return Err(error(
                "Another profile is published inside these cleanup targets; retire it from its own profile first",
            ));
        }
    }
    drop(registry);
    retire_at(&directory, &config, &data, &operator_sid()?, Some(targets))
}

fn validate_profile_data(data: &Utf8Path) -> Result<(), RunnerError> {
    if !data.is_absolute() {
        return Err(error(
            "Resource retirement requires an absolute data directory",
        ));
    }
    #[cfg(test)]
    return Ok(());
    #[cfg(all(feature = "desktop-e2e", not(test)))]
    if crate::desktop_test::current().map_err(error)?.is_some() {
        return Ok(());
    }
    #[cfg(not(test))]
    {
        let dirs = directories_next::ProjectDirs::from("net", "midi-ai-labs", "moyai")
            .ok_or_else(|| error("The default data directory is unavailable"))?;
        let expected = Utf8Path::from_path(dirs.data_dir())
            .ok_or_else(|| error("The default data directory must be UTF-8"))?;
        if data != expected {
            return Err(error(
                "A legacy publication does not identify this custom data directory; its execution evidence cannot be inferred",
            ));
        }
        Ok(())
    }
}

fn retire_at(
    directory: &Utf8Path,
    config: &Utf8Path,
    data: &Utf8Path,
    sid: &str,
    cleanup_targets: Option<&[Utf8PathBuf]>,
) -> Result<RetirementGuard, RunnerError> {
    let cleanup = cleanup_targets.is_some();
    let mut guard = RetirementGuard::empty();
    let publication_path = directory.join("publication.lock");
    let publication = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(publication_path)
        .map_err(error)?;
    fs2::FileExt::try_lock_exclusive(&publication).map_err(|_| {
        error("Local moyAI work or resource publication is still active; stop it before retirement")
    })?;
    guard._publication = Some(publication);
    let mut registry = Registry::open_at(directory, true)?;
    if let Some(targets) = cleanup_targets {
        for entry in &registry.entries {
            if targets
                .iter()
                .any(|target| within(&entry.config_path, target).unwrap_or(false))
                && entry.config_path != config
            {
                return Err(error(
                    "Another profile is published inside these cleanup targets; retire it from its own profile first",
                ));
            }
        }
    }
    let reset_directory = config.with_file_name("device-network");
    let mut reset =
        crate::device_network::reset::ResetState::load(&reset_directory).map_err(error)?;
    let selected = registry
        .entries
        .iter()
        .filter(|entry| {
            entry.config_path == config
                && (cleanup || reset.retired(&entry.hub_id, &entry.device_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !cleanup && selected.is_empty() {
        return Ok(RetirementGuard::empty());
    }
    if selected.iter().any(|entry| entry.operator_sid != sid) {
        return Err(error(
            "The published resource belongs to another Windows account",
        ));
    }
    if cleanup {
        guard._user_data =
            Some(crate::storage::maintenance::UserDataQuiescence::acquire(data).map_err(error)?);
    }
    #[cfg(windows)]
    {
        // IPC scopes hash the raw path spelling after lowercasing, whereas
        // Windows path equality also accepts alternate separators. Keep both
        // the published host and fresh current-profile startup excluded.
        let scopes = std::iter::once(config.as_str())
            .chain(selected.iter().map(|entry| entry.config_path.as_str()))
            .map(str::to_lowercase)
            .collect::<std::collections::BTreeSet<_>>();
        for scope in scopes {
            guard
                ._runner_startup
                .push(crate::runner::windows::LocalListener::reserve_for_config(
                    Utf8Path::new(&scope),
                )?);
        }
    }
    guard._journals = crate::runner::shared::lock_journals_for_retirement(data)?;
    // Old registry rows did not store data_dir. A matching durable journal in
    // the current profile is required; absent metadata is not evidence of drain.
    for entry in &selected {
        if !guard._journals.iter().any(|journal| {
            journal.hub_id.as_deref() == Some(entry.hub_id.as_str())
                && journal.device_id.as_deref() == Some(entry.device_id.as_str())
        }) {
            return Err(error(
                "The published profile's execution journal cannot be identified; preserve its registration and data",
            ));
        }
    }
    if cleanup && !selected.is_empty() {
        for entry in &selected {
            reset.retire(Some(&entry.hub_id), Some(&entry.device_id));
        }
        reset.execution_review_required = true;
        reset.save(&reset_directory).map_err(error)?;
    }
    if !selected.is_empty() {
        registry.entries.retain(|entry| !selected.contains(entry));
        registry.save()?;
    }
    guard._registry = Some(registry);
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: Utf8PathBuf,
        registry: Utf8PathBuf,
        config: Utf8PathBuf,
        data: Utf8PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let parent = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("project_sandbox/local-resource-retirement/unit");
            std::fs::create_dir_all(&parent).unwrap();
            let temp = tempfile::tempdir_in(&parent).unwrap();
            let root = Utf8Path::from_path(temp.path()).unwrap().to_owned();
            let registry = root.join("registry");
            let config = root.join("config/config.toml");
            let data = root.join("data");
            for path in [&registry, config.parent().unwrap(), &data] {
                std::fs::create_dir_all(path).unwrap();
            }
            std::fs::write(&config, "").unwrap();
            Self {
                _temp: temp,
                root,
                registry,
                config,
                data,
            }
        }

        fn entry(&self, hub: &str, device: &str) -> PublishedRoot {
            PublishedRoot {
                hub_id: hub.into(),
                device_id: device.into(),
                environment_id: String::new(),
                directory: Utf8PathBuf::new(),
                config_path: self.config.clone(),
                scope: ResourceScope::Device,
                operator_sid: "owner".into(),
            }
        }

        fn seed(&self, entries: Vec<PublishedRoot>, retired: bool) {
            let mut registry = Registry::open_at(&self.registry, true).unwrap();
            registry.entries = entries.clone();
            registry.save().unwrap();
            if retired {
                let mut reset = crate::device_network::reset::ResetState::default();
                for entry in entries
                    .iter()
                    .filter(|entry| entry.config_path == self.config)
                {
                    reset.retire(Some(&entry.hub_id), Some(&entry.device_id));
                }
                reset.pending = false;
                reset
                    .save(&self.config.with_file_name("device-network"))
                    .unwrap();
            }
        }

        fn journal(&self, hub: &str, device: &str) -> Utf8PathBuf {
            let path = self
                .data
                .join(crate::runner::shared::journal::identity_file_name(
                    hub, device,
                ));
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE runner_identity(singleton INTEGER PRIMARY KEY,hub_id TEXT NOT NULL,device_id TEXT NOT NULL);
                CREATE TABLE runner_attempts(attempt_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,entry_json TEXT NOT NULL);
                PRAGMA user_version=1;").unwrap();
            db.execute(
                "INSERT INTO runner_identity VALUES(1,?1,?2)",
                rusqlite::params![hub, device],
            )
            .unwrap();
            path
        }

        fn read(&self) -> Vec<PublishedRoot> {
            Registry::open_at(&self.registry, false).unwrap().entries
        }

        fn recover(&self) -> Result<RetirementGuard, RunnerError> {
            retire_reset_resources_at(&self.registry, &self.config, &self.data, "owner")
        }
    }

    #[test]
    fn outcome_payload_store_does_not_block_settled_resource_retirement() {
        let fixture = Fixture::new();
        fixture.seed(vec![fixture.entry("hub", "device")], true);
        fixture.journal("hub", "device");
        let payload = fixture.data.join("runner-shared-payloads.sqlite3");
        let db = rusqlite::Connection::open(&payload).unwrap();
        db.execute_batch("CREATE TABLE outcome_payloads(attempt_id TEXT PRIMARY KEY,report_sha256 TEXT NOT NULL,payload_json TEXT NOT NULL);").unwrap();
        drop(db);
        let before = std::fs::read(&payload).unwrap();
        let _guard = fixture.recover().unwrap();
        assert!(fixture.read().is_empty());
        assert_eq!(std::fs::read(payload).unwrap(), before);
    }

    #[test]
    fn retired_publication_is_removed_while_current_and_other_profiles_remain() {
        let fixture = Fixture::new();
        let old = fixture.entry("old-hub", "old-device");
        fixture.seed(vec![old.clone()], true);
        let current = fixture.entry("current-hub", "current-device");
        let mut other = fixture.entry("other-hub", "other-device");
        other.config_path = fixture.root.join("other/config.toml");
        fixture.seed(vec![old, current.clone(), other.clone()], false);
        fixture.journal("old-hub", "old-device");
        assert!(reset_publication_pending_at(&fixture.registry, &fixture.config).unwrap());
        let guard = fixture.recover().unwrap();
        assert_eq!(fixture.read(), vec![current, other]);
        assert!(!reset_publication_pending_at(&fixture.registry, &fixture.config).unwrap());
        let publication = OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.registry.join("publication.lock"))
            .unwrap();
        assert!(fs2::FileExt::try_lock_shared(&publication).is_err());
        drop(guard);
        fs2::FileExt::try_lock_shared(&publication).unwrap();
    }

    #[test]
    fn current_publication_does_not_acquire_exclusion_or_require_a_journal() {
        let fixture = Fixture::new();
        let current = fixture.entry("current-hub", "current-device");
        fixture.seed(vec![current.clone()], false);
        let publication = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(fixture.registry.join("publication.lock"))
            .unwrap();
        fs2::FileExt::try_lock_shared(&publication).unwrap();
        fixture.recover().unwrap();
        assert_eq!(fixture.read(), vec![current]);
    }

    #[test]
    fn active_resource_lease_or_journal_owner_preserves_retired_publication() {
        let fixture = Fixture::new();
        let old = fixture.entry("old-hub", "old-device");
        fixture.seed(vec![old.clone()], true);
        let path = fixture.journal("old-hub", "old-device");
        let publication = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(fixture.registry.join("publication.lock"))
            .unwrap();
        fs2::FileExt::try_lock_shared(&publication).unwrap();
        assert!(fixture.recover().is_err());
        assert_eq!(fixture.read(), vec![old.clone()]);
        drop(publication);
        let owner = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&owner).unwrap();
        assert!(fixture.recover().is_err());
        assert_eq!(fixture.read(), vec![old.clone()]);
        drop(owner);
        fixture.recover().unwrap();
        assert!(fixture.read().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn published_path_alias_runner_must_exit_before_resource_retirement() {
        let fixture = Fixture::new();
        let mut entry = fixture.entry("hub", "device");
        entry.config_path = Utf8PathBuf::from(entry.config_path.as_str().replace('\\', "/"));
        assert_eq!(entry.config_path, fixture.config);
        fixture.seed(vec![entry.clone()], true);
        fixture.journal("hub", "device");
        let old_namespace =
            crate::runner::windows::LocalListener::reserve_for_config(&entry.config_path).unwrap();
        assert!(fixture.recover().is_err());
        assert_eq!(fixture.read(), vec![entry]);
        drop(old_namespace);
        drop(fixture.recover().unwrap());
        assert!(fixture.read().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn one_runner_namespace_can_cover_multiple_published_environments() {
        let fixture = Fixture::new();
        let mut first = fixture.entry("hub", "device");
        first.config_path = Utf8PathBuf::from(first.config_path.as_str().replace('\\', "/"));
        first.scope = ResourceScope::WorkspaceIsolation { confirmed: true };
        first.environment_id = "first-environment".into();
        first.directory = fixture.root.join("first");
        let mut second = first.clone();
        second.environment_id = "second-environment".into();
        second.directory = fixture.root.join("second");
        fixture.seed(vec![first, second], true);
        fixture.journal("hub", "device");
        drop(fixture.recover().unwrap());
        assert!(fixture.read().is_empty());
    }

    #[test]
    fn foreign_owner_or_unidentified_data_is_never_assumed_drained() {
        let fixture = Fixture::new();
        let mut old = fixture.entry("old-hub", "old-device");
        old.operator_sid = "another-user".into();
        fixture.seed(vec![old.clone()], true);
        fixture.journal("old-hub", "old-device");
        assert!(fixture.recover().is_err());
        assert_eq!(fixture.read(), vec![old]);
        let old = fixture.entry("unknown-hub", "unknown-device");
        fixture.seed(vec![old.clone()], true);
        assert!(fixture.recover().is_err());
        assert_eq!(fixture.read(), vec![old]);
    }

    #[test]
    fn explicit_cleanup_keeps_exclusion_until_data_removal_finishes() {
        let fixture = Fixture::new();
        fixture.seed(vec![fixture.entry("hub", "device")], false);
        fixture.journal("hub", "device");
        let guard = retire_at(
            &fixture.registry,
            &fixture.config,
            &fixture.data,
            "owner",
            Some(&[fixture.root.clone()]),
        )
        .unwrap();
        assert!(fixture.read().is_empty());
        let reset = crate::device_network::reset::ResetState::load(
            &fixture.config.with_file_name("device-network"),
        )
        .unwrap();
        assert!(reset.retired("hub", "device"));
        std::fs::remove_dir_all(&fixture.data).unwrap();
        drop(guard);
    }

    #[test]
    fn cleanup_refuses_another_published_profile_inside_its_targets() {
        let fixture = Fixture::new();
        let mut other = fixture.entry("hub", "device");
        other.config_path = fixture.root.join("other/config.toml");
        fixture.seed(vec![other.clone()], false);
        assert!(
            retire_at(
                &fixture.registry,
                &fixture.config,
                &fixture.data,
                "owner",
                Some(&[fixture.root.clone()])
            )
            .is_err()
        );
        assert_eq!(fixture.read(), vec![other]);
        assert!(fixture.data.is_dir());
    }

    #[test]
    fn another_enrollment_cannot_replace_a_profile_before_old_publication_retires() {
        let fixture = Fixture::new();
        fixture.seed(vec![fixture.entry("old-hub", "old-device")], true);
        fixture.journal("old-hub", "old-device");
        for (hub, device) in [("new-hub", "old-device"), ("old-hub", "new-device")] {
            let settings = SharedSettings {
                version: 1,
                hub_id: hub.into(),
                device_id: device.into(),
                environments: vec![],
                resource_scope: ResourceScope::Device,
            };
            assert!(
                validate_replacement_identity(&fixture.config, &settings, &fixture.read()).is_err()
            );
        }
        let mut settings = SharedSettings {
            version: 1,
            hub_id: "old-hub".into(),
            device_id: "old-device".into(),
            environments: vec![],
            resource_scope: ResourceScope::Device,
        };
        validate_replacement_identity(&fixture.config, &settings, &fixture.read()).unwrap();
        drop(fixture.recover().unwrap());
        settings.hub_id = "new-hub".into();
        settings.device_id = "new-device".into();
        validate_replacement_identity(&fixture.config, &settings, &fixture.read()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_refuses_open_user_database_without_retiring_policy() {
        let fixture = Fixture::new();
        let published = fixture.entry("hub", "device");
        fixture.seed(vec![published.clone()], false);
        fixture.journal("hub", "device");
        let database = rusqlite::Connection::open(fixture.data.join("moyai.sqlite3")).unwrap();
        assert!(
            retire_at(
                &fixture.registry,
                &fixture.config,
                &fixture.data,
                "owner",
                Some(&[fixture.root.clone()]),
            )
            .is_err()
        );
        assert_eq!(fixture.read(), vec![published]);
        assert!(fixture.config.exists());
        let reset = crate::device_network::reset::ResetState::load(
            &fixture.config.with_file_name("device-network"),
        )
        .unwrap();
        assert!(!reset.retired("hub", "device"));
        drop(database);
        let guard = retire_at(
            &fixture.registry,
            &fixture.config,
            &fixture.data,
            "owner",
            Some(&[fixture.root.clone()]),
        )
        .unwrap();
        std::fs::remove_dir_all(&fixture.data).unwrap();
        drop(guard);
    }

    #[test]
    fn retired_or_deleted_enrollment_cannot_be_republished_by_cached_startup() {
        let fixture = Fixture::new();
        let entry = fixture.entry("hub", "device");
        fixture.seed(vec![entry], true);
        let settings = SharedSettings {
            version: 1,
            hub_id: "hub".into(),
            device_id: "device".into(),
            environments: vec![],
            resource_scope: ResourceScope::Device,
        };
        assert!(validate_registration_identity(&fixture.config, &settings).is_err());
        std::fs::remove_dir_all(fixture.config.with_file_name("device-network")).unwrap();
        assert!(validate_registration_identity(&fixture.config, &settings).is_err());
    }
}
