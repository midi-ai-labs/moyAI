//! Explicit, build-time-only isolation for real Desktop fixture processes.
//! A fixture keeps ordinary locking, but owns a separate PC identity and storage.

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

const ROOT_ENV: &str = "MOYAI_DESKTOP_E2E_ROOT";
static INSTANCE: OnceLock<Result<Option<TestInstance>, String>> = OnceLock::new();

pub(crate) struct TestInstance {
    root: Utf8PathBuf,
    suffix: String,
}

pub(crate) fn current() -> Result<Option<&'static TestInstance>, String> {
    INSTANCE
        .get_or_init(|| {
            let Some(root) = std::env::var_os(ROOT_ENV) else {
                return Ok(None);
            };
            let root = Utf8PathBuf::from_path_buf(root.into())
                .map_err(|_| "Desktop fixture root must be UTF-8".to_string())?;
            TestInstance::from_configuration(&root, |key| {
                std::env::var(key).map_err(|_| format!("Desktop fixture requires {key}"))
            })
            .map(Some)
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(Clone::clone)
}

impl TestInstance {
    fn from_configuration(
        root: &Utf8Path,
        value: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("Desktop fixture root must be absolute".into());
        }
        let root = physical_directory(root)?;
        for name in ["config", "data", "prefs", "webview", "resource-admission"] {
            let directory = physical_directory(&root.join(name))?;
            if directory.parent() != Some(root.as_path()) {
                return Err(format!(
                    "Desktop fixture directory escaped its root: {name}"
                ));
            }
        }
        for (key, relative, is_file) in [
            ("MOYAI_CONFIG_PATH", "config/config.toml", true),
            ("MOYAI_DATA_DIR", "data", false),
            ("MOYAI_DESKTOP_PREFS_PATH", "prefs/desktop.toml", true),
            ("WEBVIEW2_USER_DATA_FOLDER", "webview", false),
            ("MOYAI_TEST_RESOURCE_REGISTRY", "resource-admission", false),
        ] {
            let actual = Utf8PathBuf::from(value(key)?);
            if !actual.is_absolute() {
                return Err(format!("Desktop fixture {key} must be absolute"));
            }
            let expected = root.join(relative);
            let canonical = if is_file {
                if let Ok(metadata) = std::fs::symlink_metadata(&actual) {
                    reject_link(&actual, &metadata)?;
                    if !metadata.is_file() {
                        return Err(format!("Desktop fixture {key} must identify a file"));
                    }
                }
                let parent = actual
                    .parent()
                    .ok_or("Desktop fixture file has no parent")?;
                physical_directory(parent)?.join(actual.file_name().unwrap_or_default())
            } else {
                physical_directory(&actual)?
            };
            if path_identity(&canonical) != path_identity(&expected) {
                return Err(format!("Desktop fixture {key} must be {expected}"));
            }
        }
        let suffix = format!("{:x}", Sha256::digest(path_identity(&root).as_bytes()));
        Ok(Self { root, suffix })
    }

    pub(crate) fn lock_path(&self) -> Utf8PathBuf {
        self.root.join("config/desktop-instance.lock")
    }

    pub(crate) fn identifier(&self, base: &str) -> String {
        format!("{base}.e2e.{}", self.suffix)
    }

    pub(crate) fn resource_directory(&self) -> Utf8PathBuf {
        self.root.join("resource-admission")
    }
}

fn path_identity(path: &Utf8Path) -> String {
    #[cfg(windows)]
    {
        path.as_str().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.as_str().to_owned()
    }
}

fn reject_link(path: &Utf8Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if metadata.file_type().is_symlink() || reparse {
        return Err(format!("Desktop fixture path must not be a link: {path}"));
    }
    Ok(())
}

fn physical_directory(path: &Utf8Path) -> Result<Utf8PathBuf, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Desktop fixture directory {path}: {error}"))?;
    reject_link(path, &metadata)?;
    if !metadata.is_dir() {
        return Err(format!("Desktop fixture path must be a directory: {path}"));
    }
    path.canonicalize_utf8().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture(root: &Utf8Path) -> HashMap<&'static str, String> {
        std::fs::create_dir_all(root).unwrap();
        for directory in ["config", "data", "prefs", "webview", "resource-admission"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        [
            ("MOYAI_CONFIG_PATH", "config/config.toml"),
            ("MOYAI_DATA_DIR", "data"),
            ("MOYAI_DESKTOP_PREFS_PATH", "prefs/desktop.toml"),
            ("WEBVIEW2_USER_DATA_FOLDER", "webview"),
            ("MOYAI_TEST_RESOURCE_REGISTRY", "resource-admission"),
        ]
        .into_iter()
        .map(|(key, relative)| (key, root.join(relative).into_string()))
        .collect()
    }

    fn resolve(root: &Utf8Path, values: &HashMap<&str, String>) -> Result<TestInstance, String> {
        TestInstance::from_configuration(root, |key| {
            values.get(key).cloned().ok_or_else(|| key.into())
        })
    }

    #[test]
    fn different_pc_roots_have_distinct_stable_locks_listeners_and_resources() {
        use fs2::FileExt;
        let temp = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(temp.path()).unwrap();
        let a = base.join("a");
        let b = base.join("b");
        let a_values = fixture(&a);
        let b_values = fixture(&b);
        let a = resolve(&a, &a_values).unwrap();
        let b = resolve(&b, &b_values).unwrap();
        let again = resolve(&a.root, &a_values).unwrap();
        assert_ne!(a.identifier("moyai"), b.identifier("moyai"));
        assert_eq!(a.identifier("moyai"), again.identifier("moyai"));
        assert_ne!(a.resource_directory(), b.resource_directory());
        let open = |path| {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)
                .unwrap()
        };
        let first = open(a.lock_path());
        first.try_lock_exclusive().unwrap();
        let duplicate = open(again.lock_path());
        assert!(duplicate.try_lock_exclusive().is_err());
        let second = open(b.lock_path());
        second.try_lock_exclusive().unwrap();
    }

    #[test]
    fn incomplete_or_cross_pc_configuration_is_rejected_before_opening_storage() {
        let temp = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(temp.path()).unwrap();
        let a = base.join("a");
        let b = base.join("b");
        let values = fixture(&a);
        let other = fixture(&b);
        for key in values.keys() {
            let mut missing = values.clone();
            missing.remove(key);
            assert!(resolve(&a, &missing).is_err(), "missing {key}");
            let mut crossed = values.clone();
            crossed.insert(key, other[key].clone());
            assert!(resolve(&a, &crossed).is_err(), "crossed {key}");
        }
        let mut sibling_file = values.clone();
        sibling_file.insert(
            "MOYAI_CONFIG_PATH",
            a.join("config/other.toml").into_string(),
        );
        assert!(resolve(&a, &sibling_file).is_err());
        assert!(resolve(Utf8Path::new("relative"), &values).is_err());
        assert!(!a.join("config/desktop-instance.lock").exists());
        assert!(!a.join("data/moyai.sqlite3").exists());
    }
}
