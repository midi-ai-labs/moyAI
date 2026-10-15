//! Machine-wide publication registration, independent of a caller's config/data overrides.
//! Hub owns capacity. These records route every moyAI entry to its authenticated Runner.
use crate::runner::{
    RunnerError,
    shared::{ResourceScope, SharedSettings},
};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PublishedRoot {
    pub hub_id: String,
    pub device_id: String,
    pub environment_id: String,
    pub directory: Utf8PathBuf,
    pub config_path: Utf8PathBuf,
    pub scope: ResourceScope,
    #[serde(default)]
    pub operator_sid: String,
}

pub(super) fn error(value: impl std::fmt::Display) -> RunnerError {
    RunnerError {
        message: format!("Shared resource registration unavailable: {value}"),
    }
}
pub(crate) fn registry_directory() -> Result<Utf8PathBuf, RunnerError> {
    // Dedicated Runner process tests retain their explicit, process-local override.
    #[cfg(test)]
    if let Some(directory) = TEST_REGISTRY.get() {
        return Ok(directory.clone());
    }
    #[cfg(feature = "desktop-e2e")]
    if let Some(instance) = crate::desktop_test::current().map_err(error)? {
        return Ok(instance.resource_directory());
    }
    #[cfg(test)]
    return Ok(test_registry().clone());
    #[cfg(all(not(test), windows))]
    return super::resource_admission_windows::directory();
    #[cfg(all(not(test), not(windows)))]
    {
        let base = directories_next::BaseDirs::new()
            .ok_or_else(|| error("OS account directory is unavailable"))?;
        Utf8PathBuf::from_path_buf(
            base.data_local_dir()
                .join("moyAI")
                .join("resource-admission"),
        )
        .map_err(|_| error("OS account path is not UTF-8"))
    }
}

fn ensure_directory(directory: &Utf8Path) -> Result<(), RunnerError> {
    // This build-only scope was validated before startup; it must not create or
    // amend the real machine policy ACL when a local Desktop entry joins its Runner.
    #[cfg(all(feature = "desktop-e2e", not(test)))]
    if crate::desktop_test::current().map_err(error)?.is_some() {
        return std::fs::create_dir_all(directory).map_err(error);
    }
    #[cfg(all(windows, not(test)))]
    return super::resource_admission_windows::ensure(directory);
    #[cfg(any(test, not(windows)))]
    std::fs::create_dir_all(directory).map_err(error)
}
pub(crate) fn operator_sid() -> Result<String, RunnerError> {
    #[cfg(windows)]
    return crate::runner::windows::current_user_sid();
    #[cfg(not(windows))]
    Ok(String::new())
}

#[cfg(test)]
static TEST_REGISTRY: std::sync::OnceLock<Utf8PathBuf> = std::sync::OnceLock::new();
#[cfg(test)]
fn test_registry() -> &'static Utf8PathBuf {
    TEST_REGISTRY.get_or_init(|| {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/unit-resource-registry")
            .join(std::process::id().to_string())
    })
}
#[cfg(test)]
pub(crate) fn set_test_registry(path: Utf8PathBuf) {
    assert!(path.is_absolute(), "Fixture registry must be absolute");
    TEST_REGISTRY
        .set(path)
        .expect("Fixture registry must be selected before opening the Runner");
}
struct Registry {
    path: Utf8PathBuf,
    entries: Vec<PublishedRoot>,
    _lock: Option<File>,
}
impl Registry {
    fn open(write: bool) -> Result<Self, RunnerError> {
        let directory = registry_directory()?;
        ensure_directory(&directory)?;
        let lock = if write {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(directory.join("registry.lock"))
                .map_err(error)?;
            fs2::FileExt::try_lock_exclusive(&lock)
                .map_err(|_| error("registration is being changed; retry"))?;
            Some(lock)
        } else {
            None
        };
        let path = directory.join("registry.json");
        let entries = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                if bytes.len() > 1024 * 1024 {
                    return Err(error("registry exceeds its bound"));
                }
                serde_json::from_slice(&bytes).map_err(error)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(error(e)),
        };
        Ok(Self {
            path,
            entries,
            _lock: lock,
        })
    }
    fn save(&self) -> Result<(), RunnerError> {
        let mut file =
            tempfile::NamedTempFile::new_in(self.path.parent().expect("registry parent"))
                .map_err(error)?;
        file.write_all(&serde_json::to_vec(&self.entries).map_err(error)?)
            .map_err(error)?;
        file.as_file().sync_all().map_err(error)?;
        file.persist(&self.path).map_err(error)?;
        Ok(())
    }
}
pub(crate) fn register(settings: &SharedSettings) -> Result<(), RunnerError> {
    let config_path = crate::config::loader::global_config_path().map_err(error)?;
    let registry = Registry::open(false)?;
    let sid = operator_sid()?;
    let old = registry
        .entries
        .iter()
        .filter(|entry| entry.config_path == config_path)
        .collect::<Vec<_>>();
    let desired = publication_entries(settings, &config_path, &sid)?;
    let unchanged = old.into_iter().eq(desired.iter());
    if unchanged {
        return Ok(());
    }
    let publication = publication_file()?;
    fs2::FileExt::try_lock_exclusive(&publication).map_err(|_|error("Local moyAI work is still active; drain it before changing published resource registration"))?;
    let mut registry = Registry::open(true)?;
    for previous in &registry.entries {
        if previous.config_path != config_path
            && (matches!(previous.scope, ResourceScope::Device)
                || matches!(settings.resource_scope, ResourceScope::Device))
        {
            return Err(error(
                "Another profile provides this device. Use that Runner or explicitly configure physically isolated resources",
            ));
        }
        for mapping in &settings.environments {
            if previous.config_path != config_path
                && (within(&mapping.directory, &previous.directory)?
                    || within(&previous.directory, &mapping.directory)?)
            {
                return Err(error(
                    "This physical directory is already provided by another profile",
                ));
            }
        }
    }
    registry
        .entries
        .retain(|entry| entry.config_path != config_path);
    registry.entries.extend(desired);
    if registry.entries.len() > 1024 {
        return Err(error("registered environment capacity reached"));
    }
    registry.save()
}
fn publication_entries(
    settings: &SharedSettings,
    config_path: &Utf8Path,
    sid: &str,
) -> Result<Vec<PublishedRoot>, RunnerError> {
    if matches!(settings.resource_scope, ResourceScope::Device) {
        return Ok(vec![PublishedRoot {
            hub_id: settings.hub_id.clone(),
            device_id: settings.device_id.clone(),
            environment_id: String::new(),
            directory: Utf8PathBuf::new(),
            config_path: config_path.to_owned(),
            scope: settings.resource_scope.clone(),
            operator_sid: sid.to_owned(),
        }]);
    }
    let entries = settings
        .environments
        .iter()
        .map(|mapping| PublishedRoot {
            hub_id: settings.hub_id.clone(),
            device_id: settings.device_id.clone(),
            environment_id: mapping.environment_id.clone(),
            directory: mapping.directory.clone(),
            config_path: config_path.to_owned(),
            scope: settings.resource_scope.clone(),
            operator_sid: sid.to_owned(),
        })
        .collect::<Vec<_>>();
    Ok(entries)
}
pub(crate) struct PublicationGuard {
    _file: File,
}
pub(crate) struct ResourceGuard {
    _publication: PublicationGuard,
    _resource: File,
}
impl ResourceGuard {
    pub(crate) fn acquire(
        scope: &ResourceScope,
        directory: &Utf8Path,
    ) -> Result<Self, RunnerError> {
        use sha2::{Digest, Sha256};
        let publication = PublicationGuard::acquire()?;
        let key = match scope {
            ResourceScope::Device => "device".to_string(),
            ResourceScope::WorkspaceIsolation { .. } => {
                let physical = std::fs::canonicalize(directory).map_err(error)?;
                format!(
                    "workspace-{:x}",
                    Sha256::digest(physical.to_string_lossy().to_lowercase().as_bytes())
                )
            }
        };
        let path = registry_directory()?.join(format!("resource-{key}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(error)?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_|error("The Hub reservation cannot start while a previous local resource owner is still draining"))?;
        Ok(Self {
            _publication: publication,
            _resource: file,
        })
    }
}
fn publication_file() -> Result<File, RunnerError> {
    let directory = registry_directory()?;
    ensure_directory(&directory)?;
    let path = directory.join("publication.lock");
    match File::open(&path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
            {
                Ok(file) => Ok(file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    File::open(path).map_err(error)
                }
                Err(e) => Err(error(e)),
            }
        }
        Err(e) => Err(error(e)),
    }
}
impl PublicationGuard {
    pub(crate) fn acquire() -> Result<Self, RunnerError> {
        let file = publication_file()?;
        fs2::FileExt::try_lock_shared(&file).map_err(|_| {
            error("Resource publication is changing; retry after the operator finishes")
        })?;
        Ok(Self { _file: file })
    }
}
pub(crate) fn for_workspace(directory: &Utf8Path) -> Result<Option<PublishedRoot>, RunnerError> {
    let directory = Utf8PathBuf::from_path_buf(std::fs::canonicalize(directory).map_err(error)?)
        .map_err(|_| error("workspace path is not UTF-8"))?;
    let registry = Registry::open(false)?;
    select_binding(&directory, &registry.entries)
}
fn select_binding(
    directory: &Utf8Path,
    entries: &[PublishedRoot],
) -> Result<Option<PublishedRoot>, RunnerError> {
    if let Some(entry) = entries
        .iter()
        .find(|entry| matches!(entry.scope, ResourceScope::Device))
    {
        return Ok(Some(entry.clone()));
    }
    for entry in entries {
        if within(&directory, &entry.directory)? {
            return Ok(Some(entry.clone()));
        }
    }
    // A broader workspace must not acquire a private route around a registered subdirectory.
    let mut children = Vec::new();
    for entry in entries {
        if within(&entry.directory, &directory)? {
            children.push(entry);
        }
    }
    let mut children = children.into_iter();
    let first = children.next().cloned();
    if children.next().is_some() {
        return Err(error(
            "This workspace contains multiple independently provided resources; select one execution environment",
        ));
    }
    Ok(first)
}
fn within(candidate: &Utf8Path, root: &Utf8Path) -> Result<bool, RunnerError> {
    crate::workspace::PathGuard::security_path_is_within(candidate, root).map_err(error)
}
pub(crate) fn request_lock(id: ulid::Ulid) -> Result<File, RunnerError> {
    let directory = registry_directory()?.join("requests");
    std::fs::create_dir_all(&directory).map_err(error)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{id}.lock")))
        .map_err(error)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| error("The exact local request already has a live execution owner"))?;
    Ok(file)
}
pub(crate) fn request_is_live(id: ulid::Ulid) -> Result<bool, RunnerError> {
    let path = registry_directory()?
        .join("requests")
        .join(format!("{id}.lock"));
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(error(e)),
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(false),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(true),
        Err(e) => Err(error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_publication_contains_only_provider_routing_not_project_mappings() {
        let settings = SharedSettings {
            version: 1,
            hub_id: "hub".into(),
            device_id: "device".into(),
            resource_scope: ResourceScope::Device,
            environments: ["Alpha-confidential", "Beta-confidential"]
                .map(|id| crate::runner::shared::EnvironmentMapping {
                    environment_id: id.into(),
                    directory: Utf8PathBuf::from(format!("C:/private/{id}")),
                    access_mode: crate::config::AccessMode::Default,
                    allowed_child_environments: vec![],
                })
                .into(),
        };
        let entries = publication_entries(
            &settings,
            Utf8Path::new("C:/operator/config.toml"),
            "operator",
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].environment_id.is_empty());
        assert!(entries[0].directory.as_str().is_empty());
        assert_eq!(entries[0].config_path, "C:/operator/config.toml");
        let old:PublishedRoot=serde_json::from_value(serde_json::json!({"hub_id":"hub","device_id":"device","environment_id":"old","directory":"C:/old","config_path":"C:/operator/config.toml","scope":{"kind":"device"}})).unwrap();
        assert_eq!(old.environment_id, "old");
    }
    fn fixture() -> tempfile::TempDir {
        let path = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/resource-focused");
        std::fs::create_dir_all(&path).unwrap();
        tempfile::tempdir_in(path).unwrap()
    }
    #[test]
    fn device_binding_covers_other_roots_and_isolation_rejects_broad_ambiguous_root() {
        let temp = fixture();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let a = root.join("a");
        let b = root.join("b");
        let private = root.join("private");
        for dir in [&a, &b, &private] {
            std::fs::create_dir(dir).unwrap();
        }
        let mut entry = PublishedRoot {
            hub_id: "hub".into(),
            device_id: "device".into(),
            environment_id: "a".into(),
            directory: a.clone(),
            config_path: root.join("config.toml"),
            scope: ResourceScope::Device,
            operator_sid: "operator".into(),
        };
        assert_eq!(
            select_binding(&private, &[entry.clone()])
                .unwrap()
                .unwrap()
                .environment_id,
            "a"
        );
        entry.scope = ResourceScope::WorkspaceIsolation { confirmed: true };
        assert!(
            select_binding(&private, &[entry.clone()])
                .unwrap()
                .is_none()
        );
        let mut second = entry.clone();
        second.environment_id = "b".into();
        second.directory = b;
        assert!(select_binding(root, &[entry, second]).is_err());
    }
    #[test]
    fn physical_resource_lease_is_exclusive_and_publication_waits_for_its_drain() {
        let temp = fixture();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let resource =
            ResourceGuard::acquire(&ResourceScope::WorkspaceIsolation { confirmed: true }, root)
                .unwrap();
        assert!(
            ResourceGuard::acquire(&ResourceScope::WorkspaceIsolation { confirmed: true }, root)
                .is_err()
        );
        let publication = publication_file().unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&publication).is_err());
        drop(resource);
        fs2::FileExt::try_lock_exclusive(&publication).unwrap();
    }
    #[test]
    fn local_request_lifetime_survives_reopen_and_ends_only_when_owner_releases_it() {
        let id = ulid::Ulid::new();
        let owner = request_lock(id).unwrap();
        assert!(request_is_live(id).unwrap());
        assert!(request_lock(id).is_err());
        drop(owner);
        assert!(!request_is_live(id).unwrap());
    }
}

#[cfg(all(test, feature = "desktop-e2e"))]
mod desktop_test;
