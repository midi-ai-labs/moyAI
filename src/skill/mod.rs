use std::collections::VecDeque;
use std::fs;
use std::io::Read as _;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;

use crate::workspace::{PathGuard, skill_roots};

const SKILL_FILE_NAME: &str = "SKILL.md";
const MAX_SAMPLED_FILES: usize = 10;
const MAX_DISCOVERED_SKILLS: usize = 256;
const MAX_SKILL_DISCOVERY_VISITS: usize = 4_096;
const MAX_SKILL_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SKILL_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_SKILL_SAMPLE_VISITS: usize = 512;
const MAX_SKILL_CACHE_WORKSPACES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub description: String,
    pub path: Utf8PathBuf,
    pub base_dir: Utf8PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub manifest: DiscoveredSkill,
    pub content: String,
    pub sampled_files: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsSnapshot {
    pub workspace_root: Utf8PathBuf,
    pub roots: Vec<Utf8PathBuf>,
    pub skills: Vec<DiscoveredSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SkillsCacheKey {
    workspace_root: Utf8PathBuf,
    roots: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
struct SkillsCacheEntry {
    key: SkillsCacheKey,
    snapshot: SkillsSnapshot,
}

#[derive(Debug, Default)]
struct SkillsCache {
    entries: VecDeque<SkillsCacheEntry>,
}

#[derive(Clone, Default)]
pub struct SkillsService {
    cache: Arc<Mutex<SkillsCache>>,
}

impl SkillsService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot_for_workspace(&self, root: &Utf8Path) -> SkillsSnapshot {
        let roots = skill_roots(root);
        let key = SkillsCacheKey {
            workspace_root: root.to_path_buf(),
            roots: roots.clone(),
        };
        {
            let mut cache = self.cache.lock().expect("skills cache mutex");
            if let Some(index) = cache.entries.iter().position(|entry| entry.key == key) {
                let entry = cache
                    .entries
                    .remove(index)
                    .expect("located skills cache entry");
                let snapshot = entry.snapshot.clone();
                cache.entries.push_back(entry);
                return snapshot;
            }
        }
        let snapshot = SkillsSnapshot {
            workspace_root: root.to_path_buf(),
            roots: roots.clone(),
            skills: discover_from_roots(root, &roots),
        };
        let mut cache = self.cache.lock().expect("skills cache mutex");
        if let Some(index) = cache.entries.iter().position(|entry| entry.key == key) {
            let entry = cache
                .entries
                .remove(index)
                .expect("located concurrent skills cache entry");
            let concurrent_snapshot = entry.snapshot.clone();
            cache.entries.push_back(entry);
            return concurrent_snapshot;
        }
        if cache.entries.len() >= MAX_SKILL_CACHE_WORKSPACES {
            cache.entries.pop_front();
        }
        cache.entries.push_back(SkillsCacheEntry {
            key,
            snapshot: snapshot.clone(),
        });
        snapshot
    }

    pub fn invalidate_workspace(&self, root: &Utf8Path) {
        let mut cache = self.cache.lock().expect("skills cache mutex");
        cache
            .entries
            .retain(|entry| entry.key.workspace_root != root);
    }

    pub fn load(&self, root: &Utf8Path, name: &str) -> Result<Option<LoadedSkill>, String> {
        let snapshot = self.snapshot_for_workspace(root);
        load_from_snapshot(snapshot, name)
    }
}

pub fn discover(root: &Utf8Path) -> Vec<DiscoveredSkill> {
    discover_from_roots(root, &skill_roots(root))
}

fn discover_from_roots(root: &Utf8Path, roots: &[Utf8PathBuf]) -> Vec<DiscoveredSkill> {
    let mut skills = Vec::new();
    let mut visited_entries = 0usize;
    'roots: for skill_root in roots {
        if !skill_root.exists() {
            continue;
        }
        let mut builder = WalkBuilder::new(&skill_root);
        builder.hidden(false);
        for entry in builder.build().flatten() {
            if visited_entries >= MAX_SKILL_DISCOVERY_VISITS {
                break 'roots;
            }
            visited_entries = visited_entries.saturating_add(1);
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) else {
                continue;
            };
            if !path
                .file_name()
                .is_some_and(|file_name| file_name.eq_ignore_ascii_case(SKILL_FILE_NAME))
            {
                continue;
            }
            let Ok(text) = read_utf8_prefix(
                &path,
                std::slice::from_ref(skill_root),
                MAX_SKILL_MANIFEST_BYTES,
            ) else {
                continue;
            };
            let (name, description) = parse_skill_manifest(&path, &text);
            skills.push(DiscoveredSkill {
                name,
                description,
                base_dir: path.parent().unwrap_or(root).to_path_buf(),
                path,
            });
            if skills.len() >= MAX_DISCOVERED_SKILLS {
                break 'roots;
            }
        }
    }
    skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    skills
}

pub fn load(root: &Utf8Path, name: &str) -> Result<Option<LoadedSkill>, String> {
    load_from_snapshot(SkillsService::new().snapshot_for_workspace(root), name)
}

fn load_from_snapshot(snapshot: SkillsSnapshot, name: &str) -> Result<Option<LoadedSkill>, String> {
    let roots = snapshot.roots;
    let manifest = snapshot
        .skills
        .into_iter()
        .find(|skill| skill.name.eq_ignore_ascii_case(name));
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let content =
        read_utf8_bounded(&manifest.path, &roots, MAX_SKILL_CONTENT_BYTES).map_err(|error| {
            format!(
                "failed to read skill `{}` from {}: {error}",
                manifest.name, manifest.path
            )
        })?;
    let sampled_files = sample_related_files(&manifest.base_dir)?;
    Ok(Some(LoadedSkill {
        manifest,
        content,
        sampled_files,
    }))
}

fn read_utf8_bounded(
    path: &Utf8Path,
    roots: &[Utf8PathBuf],
    max_bytes: usize,
) -> Result<String, String> {
    let (mut file, before) = open_validated_skill_file(path, roots)?;
    if before.len() > max_bytes as u64 {
        return Err(format!("content exceeds {max_bytes} bytes"));
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    file.by_ref()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > max_bytes {
        return Err(format!("content exceeds {max_bytes} bytes"));
    }
    ensure_stable_skill_read(path, &file, &before, bytes.len() as u64, Some(before.len()))?;
    String::from_utf8(bytes).map_err(|_| "content is not valid UTF-8".to_string())
}

fn read_utf8_prefix(
    path: &Utf8Path,
    roots: &[Utf8PathBuf],
    max_bytes: usize,
) -> Result<String, String> {
    let (mut file, before) = open_validated_skill_file(path, roots)?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    file.by_ref()
        .take(max_bytes as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    ensure_stable_skill_read(
        path,
        &file,
        &before,
        bytes.len() as u64,
        Some(before.len().min(max_bytes as u64)),
    )?;
    match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(error) if error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to());
            String::from_utf8(bytes).map_err(|_| "content is not valid UTF-8".to_string())
        }
        Err(_) => Err("content is not valid UTF-8".to_string()),
    }
}

fn open_validated_skill_file(
    path: &Utf8Path,
    roots: &[Utf8PathBuf],
) -> Result<(fs::File, fs::Metadata), String> {
    let guarded = roots
        .iter()
        .find_map(|root| PathGuard::trusted_internal_path(path, root).ok())
        .ok_or_else(|| format!("skill file `{path}` is outside the configured skill roots"))?;
    let file = PathGuard::open_validated_read_file(&guarded).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!("skill file `{path}` is not a regular file"));
    }
    Ok((file, metadata))
}

fn ensure_stable_skill_read(
    path: &Utf8Path,
    file: &fs::File,
    before: &fs::Metadata,
    bytes_read: u64,
    expected_bytes: Option<u64>,
) -> Result<(), String> {
    let after = file.metadata().map_err(|error| error.to_string())?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || expected_bytes.is_some_and(|expected| bytes_read != expected)
    {
        return Err(format!(
            "skill file `{path}` changed while its contents were being read"
        ));
    }
    Ok(())
}

pub fn render_available_skills(root: &Utf8Path) -> String {
    render_available_skills_from_snapshot(&SkillsService::new().snapshot_for_workspace(root))
}

pub fn render_available_skills_from_snapshot(snapshot: &SkillsSnapshot) -> String {
    let skills = &snapshot.skills;
    if skills.is_empty() {
        return "No local skills are currently available.".to_string();
    }
    let mut lines = vec![
        "Use the `skill` tool when the current task clearly matches one of these local skills:"
            .to_string(),
    ];
    for skill in skills {
        lines.push(format!("- {}: {}", skill.name, skill.description));
    }
    lines.join("\n")
}

fn parse_skill_manifest(path: &Utf8Path, text: &str) -> (String, String) {
    let mut frontmatter_name = None;
    let mut frontmatter_description = None;
    if let Some(frontmatter) = extract_frontmatter(text) {
        for line in frontmatter.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            match key.trim() {
                "name" => frontmatter_name = parse_frontmatter_value(value),
                "description" => frontmatter_description = parse_frontmatter_value(value),
                _ => {}
            }
        }
    }

    let fallback_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(ToString::to_string)
        .unwrap_or_else(|| "skill".to_string());
    let heading_name = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim().to_string());
    let description = frontmatter_description
        .or_else(|| first_body_line(text))
        .unwrap_or_else(|| "No description.".to_string());
    let name = frontmatter_name
        .or(heading_name)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name);
    (name, description)
}

fn extract_frontmatter(text: &str) -> Option<&str> {
    let stripped = text.strip_prefix("---\n")?;
    let end = stripped.find("\n---")?;
    Some(&stripped[..end])
}

fn parse_frontmatter_value(value: &str) -> Option<String> {
    let parsed = value.trim().trim_matches('"').trim_matches('\'');
    (!parsed.is_empty()).then(|| parsed.to_string())
}

fn first_body_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && *line != "---")
        .map(ToString::to_string)
}

fn sample_related_files(base_dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, String> {
    let mut files = Vec::new();
    let mut visited_entries = 0usize;
    let mut builder = WalkBuilder::new(base_dir);
    builder.hidden(false);
    for entry in builder.build().flatten() {
        if visited_entries >= MAX_SKILL_SAMPLE_VISITS {
            break;
        }
        visited_entries = visited_entries.saturating_add(1);
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let path = Utf8PathBuf::from_path_buf(entry.into_path())
            .map_err(|_| "skill file path is not valid UTF-8".to_string())?;
        if path
            .file_name()
            .is_some_and(|file_name| file_name.eq_ignore_ascii_case(SKILL_FILE_NAME))
        {
            continue;
        }
        files.push(path);
        if files.len() >= MAX_SAMPLED_FILES {
            break;
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_for_manifest(root: &Utf8Path, manifest_path: &Utf8Path) -> SkillsSnapshot {
        SkillsSnapshot {
            workspace_root: root.to_path_buf(),
            roots: vec![root.to_path_buf()],
            skills: vec![DiscoveredSkill {
                name: "fixture".to_string(),
                description: "fixture skill".to_string(),
                path: manifest_path.to_path_buf(),
                base_dir: manifest_path
                    .parent()
                    .expect("manifest parent")
                    .to_path_buf(),
            }],
        }
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Utf8Path, link: &Utf8Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("create skill symlink fixture");
        true
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Utf8Path, link: &Utf8Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) =>
            {
                false
            }
            Err(error) => panic!("create skill symlink fixture: {error}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn cached_skill_fifo_replacement_is_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("skills")).expect("utf8 root");
        std::fs::create_dir(&root).expect("skill root");
        let manifest_path = root.join(SKILL_FILE_NAME);
        std::fs::write(&manifest_path, "---\nname: fixture\n---\n# Fixture\n")
            .expect("seed skill manifest");
        let snapshot = snapshot_for_manifest(&root, &manifest_path);
        std::fs::remove_file(&manifest_path).expect("remove discovered manifest");
        let fifo_path = std::ffi::CString::new(manifest_path.as_std_path().as_os_str().as_bytes())
            .expect("FIFO path without NUL");
        // SAFETY: `fifo_path` is a live NUL-terminated pathname and the manifest is absent.
        let created = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600 as libc::mode_t) };
        assert_eq!(
            created,
            0,
            "create raced FIFO: {}",
            std::io::Error::last_os_error()
        );

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender
                .send(load_from_snapshot(snapshot, "fixture"))
                .expect("send skill load outcome");
        });
        let mut blocked = false;
        let outcome = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                blocked = true;
                let release_deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&manifest_path)
                    {
                        Ok(writer) => {
                            drop(writer);
                            break;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                            assert!(
                                Instant::now() < release_deadline,
                                "blocked skill manifest reader could not be released"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("release blocked skill manifest reader: {error}"),
                    }
                }
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("released skill load must return")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("skill load worker disconnected before publishing its result")
            }
        };
        worker.join().expect("join skill load worker");

        assert!(!blocked, "skill load blocked while opening a raced FIFO");
        assert!(outcome.is_err(), "FIFO replacement must fail skill load");
        assert!(
            std::fs::symlink_metadata(&manifest_path)
                .expect("FIFO metadata")
                .file_type()
                .is_fifo()
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn cached_skill_and_discovery_prefix_reject_external_symlink_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let temp_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp root");
        let root = temp_root.join("skills");
        std::fs::create_dir(&root).expect("skill root");
        let manifest_path = root.join(SKILL_FILE_NAME);
        let external_path = temp_root.join("external.md");
        std::fs::write(&manifest_path, "---\nname: fixture\n---\n# Fixture\n")
            .expect("seed skill manifest");
        std::fs::write(&external_path, "external content must not be loaded")
            .expect("seed external target");
        let snapshot = snapshot_for_manifest(&root, &manifest_path);
        std::fs::remove_file(&manifest_path).expect("remove discovered manifest");
        if !create_file_symlink(&external_path, &manifest_path) {
            return;
        }

        let roots = vec![root.clone()];
        let prefix_error = read_utf8_prefix(&manifest_path, &roots, MAX_SKILL_MANIFEST_BYTES)
            .expect_err("discovery prefix must not follow an external symlink");
        let load_error = load_from_snapshot(snapshot, "fixture")
            .expect_err("cached skill load must not follow an external symlink");

        assert!(!prefix_error.contains("external content must not be loaded"));
        assert!(!load_error.contains("external content must not be loaded"));
        assert_eq!(
            std::fs::read_to_string(&external_path).expect("read untouched external target"),
            "external content must not be loaded"
        );
    }

    #[test]
    fn workspace_snapshot_cache_is_bounded_and_recently_used() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        let service = SkillsService::new();
        let workspaces = (0..=MAX_SKILL_CACHE_WORKSPACES)
            .map(|index| root.join(format!("workspace-{index}")))
            .collect::<Vec<_>>();

        for workspace in &workspaces {
            service.snapshot_for_workspace(workspace);
        }

        let cache = service.cache.lock().expect("skills cache mutex");
        assert_eq!(cache.entries.len(), MAX_SKILL_CACHE_WORKSPACES);
        assert!(
            cache
                .entries
                .iter()
                .all(|entry| entry.key.workspace_root != workspaces[0])
        );
        assert!(
            cache
                .entries
                .iter()
                .any(|entry| entry.key.workspace_root == workspaces[MAX_SKILL_CACHE_WORKSPACES])
        );
        drop(cache);

        service.snapshot_for_workspace(&workspaces[1]);
        let cache = service.cache.lock().expect("skills cache mutex");
        assert_eq!(
            cache.entries.back().map(|entry| &entry.key.workspace_root),
            Some(&workspaces[1])
        );
    }
}
