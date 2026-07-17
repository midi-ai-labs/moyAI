use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ToolError;
use crate::workspace::Workspace;

const CURSOR_PREFIX: &str = "walk-v3:";
const TRAVERSAL_SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_ACTIVE_TRAVERSAL_SNAPSHOTS: usize = 64;
const MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT: usize = 8_192;
const MAX_TRAVERSAL_DIRECTORIES_TOTAL: usize = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversalOptions {
    pub include_hidden: bool,
    pub max_depth: Option<usize>,
    pub include_files: bool,
    pub include_directories: bool,
    pub result_limit: usize,
    pub visit_limit: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TraversalRegistry {
    inner: Arc<Mutex<TraversalRegistryState>>,
}

#[derive(Debug, Default)]
struct TraversalRegistryState {
    snapshots: HashMap<String, TraversalSnapshot>,
}

#[derive(Debug)]
struct TraversalSnapshot {
    root: Utf8PathBuf,
    options: TraversalOptions,
    generation: u64,
    directory_stamps: BTreeMap<Utf8PathBuf, DirectoryStamp>,
    admissible_resumes: HashMap<String, Utf8PathBuf>,
    last_used: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    modified_nanos: u128,
    created_nanos: Option<u128>,
    metadata_len: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TraversalCursorPayload {
    snapshot_id: String,
    generation: u64,
    resume_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalEntry {
    pub path: Utf8PathBuf,
    pub relative_path: Utf8PathBuf,
    pub is_directory: bool,
    pub depth: usize,
    pub cursor: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalPage {
    pub entries: Vec<TraversalEntry>,
    pub continuation: Option<String>,
    pub truncated: bool,
    pub visited_entries: usize,
}

pub fn walk_page(
    root: &Utf8Path,
    workspace: &Workspace,
    cursor: Option<&str>,
    options: TraversalOptions,
) -> Result<TraversalPage, ToolError> {
    let (snapshot_id, mut snapshot, resume_path) = workspace
        .traversal_registry
        .checkout(root, cursor, options)?;
    validate_directory_stamps(&snapshot.directory_stamps)?;
    let result_limit = options.result_limit.max(1);
    let visit_limit = options.visit_limit.max(1);
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(!options.include_hidden);
    builder.git_ignore(workspace.ignore.use_gitignore);
    builder.max_depth(options.max_depth);
    builder.sort_by_file_path(|left, right| left.cmp(right));
    if let Some(resume_path) = resume_path.clone() {
        let traversal_root = root.to_path_buf();
        builder.filter_entry(move |entry| {
            let path = entry.path();
            path == traversal_root.as_std_path()
                || resume_path.as_std_path().starts_with(path)
                || path >= resume_path.as_std_path()
        });
    }

    let mut entries = Vec::new();
    let mut visited_entries = 0usize;
    let mut continuation = None;
    let mut exhausted = true;

    for entry in builder.build() {
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir())
        {
            record_directory_stamp(&mut snapshot.directory_stamps, &path)?;
        }
        if resume_path.as_ref().is_some_and(|resume| path < *resume) {
            continue;
        }
        if visited_entries >= visit_limit {
            continuation = Some(path);
            exhausted = false;
            break;
        }
        visited_entries = visited_entries.saturating_add(1);
        if path == root
            || workspace
                .protected_paths
                .iter()
                .any(|protected| path.starts_with(protected))
            || workspace
                .ignore
                .matches_compiled(&ignore, &workspace.root, &path)
        {
            continue;
        }
        let is_directory = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir());
        let is_file = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file());
        if (!is_directory || !options.include_directories) && (!is_file || !options.include_files) {
            continue;
        }
        if entries.len() >= result_limit {
            continuation = Some(path);
            exhausted = false;
            break;
        }
        let relative_path = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        entries.push(TraversalEntry {
            depth: relative_path.components().count(),
            relative_path,
            path,
            is_directory,
            cursor: String::new(),
        });
    }

    validate_directory_stamps(&snapshot.directory_stamps)?;
    snapshot.admissible_resumes.clear();
    for entry in &mut entries {
        entry.cursor = register_resume_cursor(&snapshot_id, &mut snapshot, &entry.path)?;
    }
    let continuation = continuation
        .as_ref()
        .map(|path| register_resume_cursor(&snapshot_id, &mut snapshot, path))
        .transpose()?;
    if !entries.is_empty() || continuation.is_some() {
        workspace.traversal_registry.store(snapshot_id, snapshot)?;
    }

    Ok(TraversalPage {
        entries,
        truncated: !exhausted,
        continuation,
        visited_entries,
    })
}

impl TraversalRegistry {
    fn checkout(
        &self,
        root: &Utf8Path,
        cursor: Option<&str>,
        options: TraversalOptions,
    ) -> Result<(String, TraversalSnapshot, Option<Utf8PathBuf>), ToolError> {
        let cursor = cursor.map(str::trim).filter(|value| !value.is_empty());
        let Some(cursor) = cursor else {
            let snapshot_id = ulid::Ulid::new().to_string();
            return Ok((
                snapshot_id,
                TraversalSnapshot {
                    root: root.to_path_buf(),
                    options,
                    generation: 1,
                    directory_stamps: BTreeMap::new(),
                    admissible_resumes: HashMap::new(),
                    last_used: Instant::now(),
                },
                None,
            ));
        };
        let payload = decode_cursor_payload(cursor)?;
        let mut registry = self
            .inner
            .lock()
            .expect("traversal registry mutex poisoned");
        registry.prune_expired();
        let stored = registry
            .snapshots
            .get(&payload.snapshot_id)
            .ok_or_else(stale_cursor_error)?;
        if stored.root != root {
            return Err(ToolError::Message(
                "traversal cursor belongs to a different root".to_string(),
            ));
        }
        if stored.options != options {
            return Err(ToolError::Message(
                "traversal cursor options do not match the original traversal".to_string(),
            ));
        }
        if stored.generation != payload.generation {
            return Err(stale_cursor_error());
        }
        let resume_path = stored
            .admissible_resumes
            .get(&payload.resume_key)
            .cloned()
            .ok_or_else(stale_cursor_error)?;
        let mut snapshot = registry
            .snapshots
            .remove(&payload.snapshot_id)
            .expect("checked traversal snapshot must still exist");
        snapshot.generation = snapshot.generation.checked_add(1).ok_or_else(|| {
            ToolError::Message("traversal cursor generation exhausted".to_string())
        })?;
        snapshot.last_used = Instant::now();
        Ok((payload.snapshot_id, snapshot, Some(resume_path)))
    }

    fn store(&self, snapshot_id: String, mut snapshot: TraversalSnapshot) -> Result<(), ToolError> {
        if snapshot.directory_stamps.len() > MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT {
            return Err(ToolError::Message(format!(
                "traversal snapshot exceeded its directory fence limit of {MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT}; narrow the path or max_depth"
            )));
        }
        snapshot.last_used = Instant::now();
        let mut registry = self
            .inner
            .lock()
            .expect("traversal registry mutex poisoned");
        registry.prune_expired();
        registry.make_room_for(snapshot.directory_stamps.len());
        let retained_directories = registry
            .snapshots
            .values()
            .map(|value| value.directory_stamps.len())
            .sum::<usize>();
        if registry.snapshots.len() >= MAX_ACTIVE_TRAVERSAL_SNAPSHOTS
            || retained_directories.saturating_add(snapshot.directory_stamps.len())
                > MAX_TRAVERSAL_DIRECTORIES_TOTAL
        {
            return Err(ToolError::Message(
                "traversal snapshot registry is full; restart this traversal after older cursors expire"
                    .to_string(),
            ));
        }
        registry.snapshots.insert(snapshot_id, snapshot);
        Ok(())
    }
}

impl TraversalRegistryState {
    fn prune_expired(&mut self) {
        self.snapshots
            .retain(|_, snapshot| snapshot.last_used.elapsed() <= TRAVERSAL_SNAPSHOT_TTL);
    }

    fn make_room_for(&mut self, incoming_directories: usize) {
        loop {
            let retained_directories = self
                .snapshots
                .values()
                .map(|value| value.directory_stamps.len())
                .sum::<usize>();
            if self.snapshots.len() < MAX_ACTIVE_TRAVERSAL_SNAPSHOTS
                && retained_directories.saturating_add(incoming_directories)
                    <= MAX_TRAVERSAL_DIRECTORIES_TOTAL
            {
                break;
            }
            let Some(oldest) = self
                .snapshots
                .iter()
                .min_by_key(|(_, snapshot)| snapshot.last_used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.snapshots.remove(&oldest);
        }
    }
}

fn decode_cursor_payload(cursor: &str) -> Result<TraversalCursorPayload, ToolError> {
    let value = cursor
        .strip_prefix(CURSOR_PREFIX)
        .ok_or_else(|| ToolError::Message("invalid traversal cursor version".to_string()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ToolError::Message("invalid traversal cursor payload".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ToolError::Message("invalid traversal cursor payload".to_string()))
}

fn register_resume_cursor(
    snapshot_id: &str,
    snapshot: &mut TraversalSnapshot,
    path: &Utf8Path,
) -> Result<String, ToolError> {
    if path == snapshot.root || !path.starts_with(&snapshot.root) {
        return Err(ToolError::Message(
            "traversal continuation is outside its root".to_string(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(snapshot_id.as_bytes());
    hasher.update(snapshot.generation.to_le_bytes());
    hasher.update(path.as_str().as_bytes());
    let resume_key = format!("{:x}", hasher.finalize());
    snapshot
        .admissible_resumes
        .insert(resume_key.clone(), path.to_path_buf());
    let payload = serde_json::to_vec(&TraversalCursorPayload {
        snapshot_id: snapshot_id.to_string(),
        generation: snapshot.generation,
        resume_key,
    })
    .map_err(|error| ToolError::Message(format!("failed to encode traversal cursor: {error}")))?;
    Ok(format!(
        "{CURSOR_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(payload)
    ))
}

fn stale_cursor_error() -> ToolError {
    ToolError::Message(
        "traversal cursor expired, was already consumed, or was superseded".to_string(),
    )
}

fn record_directory_stamp(
    stamps: &mut BTreeMap<Utf8PathBuf, DirectoryStamp>,
    path: &Utf8Path,
) -> Result<(), ToolError> {
    if stamps.contains_key(path) {
        return Ok(());
    }
    if stamps.len() >= MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT {
        return Err(ToolError::Message(format!(
            "traversal snapshot exceeded its directory fence limit of {MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT}; narrow the path or max_depth"
        )));
    }
    stamps.insert(path.to_path_buf(), directory_stamp(path)?);
    Ok(())
}

fn validate_directory_stamps(
    stamps: &BTreeMap<Utf8PathBuf, DirectoryStamp>,
) -> Result<(), ToolError> {
    for (path, expected) in stamps {
        let current = directory_stamp(path).map_err(|_| {
            ToolError::Message(format!(
                "traversal snapshot changed because directory `{path}` is no longer available; restart the traversal"
            ))
        })?;
        if &current != expected {
            return Err(ToolError::Message(format!(
                "traversal snapshot changed at directory `{path}`; restart the traversal to avoid omitted or duplicated paths"
            )));
        }
    }
    Ok(())
}

fn directory_stamp(path: &Utf8Path) -> Result<DirectoryStamp, ToolError> {
    let metadata = fs::metadata(path).map_err(|error| {
        ToolError::Message(format!(
            "failed to read traversal directory metadata for `{path}`: {error}"
        ))
    })?;
    if !metadata.is_dir() {
        return Err(ToolError::Message(format!(
            "traversal fence path `{path}` is no longer a directory"
        )));
    }
    let modified = metadata.modified().map_err(|error| {
        ToolError::Message(format!(
            "filesystem does not expose a traversal modification fence for `{path}`: {error}"
        ))
    })?;
    let modified_nanos = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            ToolError::Message(format!(
                "traversal modification time for `{path}` predates the Unix epoch"
            ))
        })?
        .as_nanos();
    let created_nanos = metadata
        .created()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos());
    Ok(DirectoryStamp {
        modified_nanos,
        created_nanos,
        metadata_len: metadata.len(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use camino::Utf8PathBuf;

    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    use super::{TraversalOptions, walk_page};

    #[test]
    fn traversal_stops_at_the_page_boundary_and_resumes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).expect("write fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 2,
            visit_limit: 16,
        };

        let first = walk_page(&root, &workspace, None, options).expect("first page");
        assert_eq!(first.entries.len(), 2);
        assert!(first.truncated);
        let second = walk_page(&root, &workspace, first.continuation.as_deref(), options)
            .expect("second page");

        assert_eq!(second.entries.len(), 1);
        assert!(!second.truncated);
        let mut names = first
            .entries
            .iter()
            .chain(second.entries.iter())
            .map(|entry| entry.relative_path.to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn visit_budget_returns_a_continuation_without_materializing_the_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for index in 0..20 {
            std::fs::write(root.join(format!("{index:02}.txt")), index.to_string())
                .expect("write fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let page = walk_page(
            &root,
            &workspace,
            None,
            TraversalOptions {
                include_hidden: false,
                max_depth: None,
                include_files: true,
                include_directories: true,
                result_limit: 100,
                visit_limit: 4,
            },
        )
        .expect("bounded page");

        assert_eq!(page.visited_entries, 4);
        assert!(page.truncated);
        assert!(page.continuation.is_some());
    }

    #[test]
    fn continuation_is_scoped_to_the_original_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let other_root = root.join("other");
        std::fs::create_dir_all(&other_root).expect("other root");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), name).expect("fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: true,
            result_limit: 1,
            visit_limit: 16,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");

        let error = walk_page(&other_root, &workspace, Some(&cursor), options)
            .expect_err("cursor root mismatch must be rejected");

        assert!(error.to_string().contains("different root"));
    }

    #[test]
    fn continuation_rejects_a_changed_directory_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).expect("fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 16,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("before-cursor.txt"), "changed").expect("mutate root");

        let error = walk_page(&root, &workspace, Some(&cursor), options)
            .expect_err("changed snapshot must be rejected");

        assert!(error.to_string().contains("snapshot changed"));
    }

    #[test]
    fn continuation_cursor_is_single_use() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).expect("fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 16,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");
        walk_page(&root, &workspace, Some(&cursor), options).expect("consume cursor");

        let error = walk_page(&root, &workspace, Some(&cursor), options)
            .expect_err("stale cursor must be rejected");

        assert!(error.to_string().contains("expired"));
    }
}
