//! Quiescence shared by profile relocation and product-scoped data removal.
use super::*;
use crate::runner::shared::RetirementJournalGuard;
use crate::storage::maintenance::UserDataQuiescence;

pub(crate) struct ProfileMaintenanceGuard {
    registry: Registry,
    user_data: Vec<UserDataQuiescence>,
    _journals: Vec<RetirementJournalGuard>,
    #[cfg(windows)]
    _runners: Vec<crate::runner::windows::LocalListener>,
    // Keep publication exclusive until all other evidence leases are released.
    _publication: File,
}

impl ProfileMaintenanceGuard {
    #[cfg(test)]
    pub(crate) fn for_test(
        directory: &Utf8Path,
        profiles: &[(Utf8PathBuf, Utf8PathBuf)],
        targets: &[Utf8PathBuf],
    ) -> Result<Self, RunnerError> {
        Self::acquire_at(directory, profiles, targets, "owner", false)
    }
    pub(crate) fn acquire(
        profiles: &[(Utf8PathBuf, Utf8PathBuf)],
        targets: &[Utf8PathBuf],
    ) -> Result<Self, RunnerError> {
        Self::acquire_at(
            &registry_directory()?,
            profiles,
            targets,
            &operator_sid()?,
            false,
        )
    }

    pub(crate) fn acquire_for_reset(
        profiles: &[(Utf8PathBuf, Utf8PathBuf)],
        targets: &[Utf8PathBuf],
    ) -> Result<Self, RunnerError> {
        Self::acquire_at(
            &registry_directory()?,
            profiles,
            targets,
            &operator_sid()?,
            true,
        )
    }

    fn acquire_at(
        directory: &Utf8Path,
        profiles: &[(Utf8PathBuf, Utf8PathBuf)],
        targets: &[Utf8PathBuf],
        sid: &str,
        cleanup: bool,
    ) -> Result<Self, RunnerError> {
        ensure_directory(directory)?;
        let publication = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("publication.lock"))
            .map_err(error)?;
        fs2::FileExt::try_lock_exclusive(&publication)
            .map_err(|_| error("moyAI is still running; close Desktop, CLI and Runner before moving or removing saved data"))?;
        let registry = Registry::open_at(directory, true)?;
        let selected = registry
            .entries
            .iter()
            .filter(|entry| {
                profiles
                    .iter()
                    .any(|(config, _)| same_path(config, &entry.config_path))
            })
            .collect::<Vec<_>>();
        for entry in &registry.entries {
            if targets.iter().any(|target| {
                within_owned(&entry.config_path, target) || within_owned(&entry.directory, target)
            }) && !selected.contains(&entry)
            {
                return Err(error(
                    "Another profile uses the selected data directory; preserve its data and registration",
                ));
            }
        }
        if selected.iter().any(|entry| entry.operator_sid != sid) {
            return Err(error(
                "The selected publication belongs to another Windows account",
            ));
        }
        let mut user_data = Vec::new();
        let mut journals = Vec::new();
        #[cfg(windows)]
        let mut runners = Vec::new();
        #[cfg(windows)]
        {
            let scopes = profiles
                .iter()
                .map(|(config, _)| config.as_str())
                .chain(selected.iter().map(|entry| entry.config_path.as_str()))
                .map(str::to_lowercase)
                .collect::<std::collections::BTreeSet<_>>();
            for scope in scopes {
                runners.push(crate::runner::windows::LocalListener::reserve_for_config(
                    Utf8Path::new(&scope),
                )?);
            }
        }
        for (config, data) in profiles {
            user_data.push(
                if cleanup {
                    UserDataQuiescence::acquire_for_cleanup(data, targets)
                } else {
                    UserDataQuiescence::acquire(data)
                }
                .map_err(error)?,
            );
            let profile_journals = crate::runner::shared::lock_journals_for_retirement(data)?;
            for entry in selected
                .iter()
                .filter(|entry| same_path(&entry.config_path, config))
            {
                if !profile_journals.iter().any(|journal| {
                    journal.hub_id.as_deref() == Some(&entry.hub_id)
                        && journal.device_id.as_deref() == Some(&entry.device_id)
                }) {
                    return Err(error(
                        "The selected profile's execution journal cannot prove safe maintenance",
                    ));
                }
            }
            journals.extend(profile_journals);
        }
        Ok(Self {
            registry,
            user_data,
            _journals: journals,
            #[cfg(windows)]
            _runners: runners,
            _publication: publication,
        })
    }

    pub(crate) fn release_database_for_migration(&mut self) {
        for guard in &mut self.user_data {
            guard.release_database_for_migration();
        }
    }

    pub(crate) fn relocate(
        &mut self,
        mappings: &[(Utf8PathBuf, Utf8PathBuf)],
    ) -> Result<(), RunnerError> {
        let before = self.registry.entries.clone();
        for entry in &mut self.registry.entries {
            entry.config_path = relocated(&entry.config_path, mappings);
            entry.directory = relocated(&entry.directory, mappings);
        }
        if self.registry.entries != before {
            if let Err(error) = self.registry.save() {
                self.registry.entries = before;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn retire(
        &mut self,
        profiles: &[(Utf8PathBuf, Utf8PathBuf)],
    ) -> Result<(), RunnerError> {
        for (config, _) in profiles {
            let selected = self
                .registry
                .entries
                .iter()
                .filter(|entry| same_path(&entry.config_path, config))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let directory = config.with_file_name("device-network");
            let mut reset =
                crate::device_network::reset::ResetState::load(&directory).map_err(error)?;
            for entry in selected {
                reset.retire(Some(&entry.hub_id), Some(&entry.device_id));
            }
            reset.execution_review_required = true;
            reset.save(&directory).map_err(error)?;
        }
        let before = self.registry.entries.clone();
        self.registry.entries.retain(|entry| {
            !profiles
                .iter()
                .any(|(config, _)| same_path(config, &entry.config_path))
        });
        if self.registry.entries != before {
            self.registry.save()?;
        }
        Ok(())
    }
}

pub(crate) fn relocated(path: &Utf8Path, mappings: &[(Utf8PathBuf, Utf8PathBuf)]) -> Utf8PathBuf {
    for (from, to) in mappings {
        if let Ok(relative) = path.strip_prefix(from) {
            return to.join(relative);
        }
        #[cfg(windows)]
        {
            let path_spelling = windows_spelling(path);
            let from_spelling = windows_spelling(from);
            let path_parts = path_spelling.split('/').collect::<Vec<_>>();
            let from_parts = from_spelling
                .trim_end_matches('/')
                .split('/')
                .collect::<Vec<_>>();
            if path_parts.len() >= from_parts.len()
                && path_parts
                    .iter()
                    .zip(&from_parts)
                    .all(|(left, right)| left.eq_ignore_ascii_case(right))
            {
                return to.join(path_parts[from_parts.len()..].join("/"));
            }
        }
    }
    path.to_owned()
}

fn same_path(left: &Utf8Path, right: &Utf8Path) -> bool {
    #[cfg(windows)]
    {
        windows_spelling(left).eq_ignore_ascii_case(&windows_spelling(right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn windows_spelling(path: &Utf8Path) -> String {
    let spelling = path.as_str().replace('\\', "/");
    spelling
        .strip_prefix("//?/")
        .unwrap_or(&spelling)
        .to_owned()
}

fn within_owned(path: &Utf8Path, root: &Utf8Path) -> bool {
    if same_path(path, root) {
        return true;
    }
    let relocated = relocated(path, &[(root.to_owned(), Utf8PathBuf::from("__owned__"))]);
    relocated.starts_with("__owned__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_refuses_registered_workspace_before_retiring_its_publication() {
        use crate::session::{ProjectId, ProjectRepository};
        let fixture = Fixture::new();
        fixture.journal();
        let entry = fixture.entry();
        fixture.seed(vec![entry.clone()]);
        let project = fixture.data.join("user-project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("keep.txt"), "user work").unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: fixture.data.clone(),
            database_path: fixture.data.join("moyai.sqlite3"),
            truncation_dir: fixture.data.join("truncation"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let store = crate::storage::StoreBundle::new(sqlite);
        store
            .project_repo()
            .upsert_project(ProjectId::new(), &project, "project", "none")
            .await
            .unwrap();
        drop(store);
        let failure = ProfileMaintenanceGuard::acquire_at(
            &fixture.registry,
            &[(fixture.config.clone(), fixture.data.clone())],
            &[fixture.root.join("old")],
            "owner",
            true,
        )
        .err()
        .unwrap();
        assert!(failure.message.contains(project.as_str()));
        assert_eq!(fixture.read(), [entry]);
        assert_eq!(
            std::fs::read_to_string(project.join("keep.txt")).unwrap(),
            "user work"
        );
        assert!(!fixture.config.with_file_name("device-network").exists());
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: Utf8PathBuf,
        registry: Utf8PathBuf,
        config: Utf8PathBuf,
        data: Utf8PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = crate::user_data::test_directory();
            let root = Utf8Path::from_path(temp.path()).unwrap().to_owned();
            let registry = root.join("machine");
            let config = root.join("old/config/config.toml");
            let data = root.join("old/data");
            for path in [&registry, config.parent().unwrap(), &data] {
                std::fs::create_dir_all(path).unwrap();
            }
            Self {
                _temp: temp,
                root,
                registry,
                config,
                data,
            }
        }
        fn entry(&self) -> PublishedRoot {
            PublishedRoot {
                hub_id: "hub".into(),
                device_id: "device".into(),
                environment_id: String::new(),
                directory: Utf8PathBuf::new(),
                config_path: self.config.clone(),
                scope: ResourceScope::Device,
                operator_sid: "owner".into(),
            }
        }
        fn seed(&self, entries: Vec<PublishedRoot>) {
            let mut registry = Registry::open_at(&self.registry, true).unwrap();
            registry.entries = entries;
            registry.save().unwrap();
        }
        fn journal(&self) {
            let db = rusqlite::Connection::open(self.data.join("runner-shared.sqlite3")).unwrap();
            db.execute_batch("CREATE TABLE runner_identity(singleton INTEGER PRIMARY KEY,hub_id TEXT NOT NULL,device_id TEXT NOT NULL); CREATE TABLE runner_attempts(attempt_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,entry_json TEXT NOT NULL); PRAGMA user_version=1; INSERT INTO runner_identity VALUES(1,'hub','device');").unwrap();
        }
        fn acquire(&self) -> Result<ProfileMaintenanceGuard, RunnerError> {
            ProfileMaintenanceGuard::for_test(
                &self.registry,
                &[(self.config.clone(), self.data.clone())],
                &[self.root.join("old")],
            )
        }
        fn read(&self) -> Vec<PublishedRoot> {
            Registry::open_at(&self.registry, false).unwrap().entries
        }
    }

    #[test]
    fn relocation_preserves_identity_and_foreign_publication() {
        let f = Fixture::new();
        f.journal();
        let entry = f.entry();
        let mut foreign = entry.clone();
        foreign.config_path = f.root.join("other/config.toml");
        foreign.device_id = "other-device".into();
        foreign.operator_sid = "other-account".into();
        f.seed(vec![entry.clone(), foreign.clone()]);
        let mut guard = f.acquire().unwrap();
        guard
            .relocate(&[(f.root.join("old"), f.root.join("new/desktop"))])
            .unwrap();
        let entries = f.read();
        assert_eq!(
            entries[0].config_path,
            f.root.join("new/desktop/config/config.toml")
        );
        assert_eq!(entries[0].hub_id, entry.hub_id);
        assert_eq!(entries[0].device_id, entry.device_id);
        assert_eq!(entries[1], foreign);
    }

    #[test]
    fn cleanup_retires_only_proven_profiles_and_holds_exclusion() {
        let f = Fixture::new();
        f.journal();
        f.seed(vec![f.entry()]);
        let mut guard = f.acquire().unwrap();
        guard.retire(&[(f.config.clone(), f.data.clone())]).unwrap();
        assert!(f.read().is_empty());
        let reset = crate::device_network::reset::ResetState::load(
            &f.config.with_file_name("device-network"),
        )
        .unwrap();
        assert!(reset.retired("hub", "device"));
        let publication = std::fs::File::open(f.registry.join("publication.lock")).unwrap();
        assert!(fs2::FileExt::try_lock_shared(&publication).is_err());
        drop(guard);
        fs2::FileExt::try_lock_shared(&publication).unwrap();
    }

    #[test]
    fn unknown_journal_and_foreign_account_preserve_registration() {
        let f = Fixture::new();
        let entry = f.entry();
        f.seed(vec![entry.clone()]);
        assert!(f.acquire().is_err());
        assert_eq!(f.read(), [entry.clone()]);
        f.journal();
        let mut foreign = entry;
        foreign.operator_sid = "other-account".into();
        f.seed(vec![foreign.clone()]);
        assert!(f.acquire().is_err());
        assert_eq!(f.read(), [foreign]);
    }

    #[test]
    fn live_desktop_prevents_maintenance_without_mutating_registration() {
        let f = Fixture::new();
        f.journal();
        let entry = f.entry();
        f.seed(vec![entry.clone()]);
        let desktop = std::fs::File::create(
            f.data
                .join(crate::storage::maintenance::DESKTOP_INSTANCE_LOCK),
        )
        .unwrap();
        fs2::FileExt::try_lock_exclusive(&desktop).unwrap();
        assert!(f.acquire().is_err());
        assert_eq!(f.read(), [entry]);
        drop(desktop);
        assert!(f.acquire().is_ok());
    }

    #[test]
    fn another_profile_using_a_selected_directory_is_preserved() {
        let f = Fixture::new();
        let mut entry = f.entry();
        entry.config_path = f.root.join("other/config.toml");
        entry.directory = f.data.join("workspace");
        f.seed(vec![entry.clone()]);
        assert!(f.acquire().is_err());
        assert_eq!(f.read(), [entry]);
    }
    #[test]
    fn relocation_only_changes_complete_owned_path_prefixes() {
        let from = Utf8PathBuf::from("C:/old/moyai");
        let to = Utf8PathBuf::from("C:/new/desktop");
        let mappings = [(from.clone(), to.clone())];
        assert_eq!(
            relocated(&from.join("data/db"), &mappings),
            to.join("data/db")
        );
        assert_eq!(
            relocated(Utf8Path::new("C:/old/moyai-other/db"), &mappings),
            Utf8PathBuf::from("C:/old/moyai-other/db")
        );
    }
}
