use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead as _, Read as _};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ToolError;
use crate::workspace::path_guard::ExistingObjectIdentity;
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace};

const CURSOR_PREFIX: &str = "walk-v3:";
const TRAVERSAL_SNAPSHOT_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_ACTIVE_TRAVERSAL_SNAPSHOTS: usize = 64;
const MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT: usize = 8_192;
const MAX_TRAVERSAL_DIRECTORIES_TOTAL: usize = 32_768;
const MAX_ACTIVE_ONE_SHOT_CONTINUATIONS: usize = 512;
const MAX_ONE_SHOT_CONTINUATION_BYTES: usize = 16 * 1024;
const MAX_TRAVERSAL_IGNORE_SOURCES_PER_SNAPSHOT: usize =
    MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT * 2 + 512;
const MAX_IGNORE_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_IGNORE_SOURCE_BYTES_PER_SNAPSHOT: u64 = 8 * 1024 * 1024;
const MAX_IGNORE_ANCESTOR_DIRECTORIES: usize = 256;

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
    one_shot_continuations: HashMap<String, OneShotContinuation>,
}

#[derive(Debug)]
struct TraversalSnapshot {
    root: Utf8PathBuf,
    root_identity: ExistingObjectIdentity,
    options: TraversalOptions,
    generation: u64,
    directory_stamps: BTreeMap<Utf8PathBuf, DirectoryStamp>,
    ignore_plan_sha256: [u8; 32],
    ignore_sources: BTreeMap<Utf8PathBuf, IgnoreSourceStamp>,
    ignore_repository_boundaries: BTreeMap<Utf8PathBuf, bool>,
    ignore_source_bytes: u64,
    admissible_resumes: HashMap<String, Utf8PathBuf>,
    last_used: Instant,
}

#[derive(Debug)]
struct OneShotContinuation {
    domain: String,
    payload: Vec<u8>,
    last_used: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryStamp {
    object_identity: ExistingObjectIdentity,
    modified_nanos: u128,
    created_nanos: Option<u128>,
    metadata_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IgnoreSourceKind {
    Missing,
    RegularFile,
    SymlinkFile,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IgnoreSourceStamp {
    kind: IgnoreSourceKind,
    content_len: u64,
    content_sha256: [u8; 32],
}

#[derive(Debug)]
struct CapturedIgnoreSource {
    stamp: IgnoreSourceStamp,
    matcher: Option<Gitignore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IgnoreDecision {
    Ignore,
    Whitelist,
}

#[derive(Debug)]
struct IgnorePageState {
    sources: BTreeMap<Utf8PathBuf, CapturedIgnoreSource>,
    repository_boundaries: BTreeMap<Utf8PathBuf, bool>,
    source_bytes: u64,
    use_gitignore: bool,
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
    pub size_bytes: Option<u64>,
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

fn compare_traversal_path(left: &Path, right: &Path) -> Ordering {
    left.cmp(right)
}

pub fn walk_page(
    root: &Utf8Path,
    workspace: &Workspace,
    cursor: Option<&str>,
    options: TraversalOptions,
) -> Result<TraversalPage, ToolError> {
    let guarded = PathGuard::require_path(workspace, root, AccessKind::Search)?;
    walk_guarded_page(&guarded, workspace, cursor, options)
}

pub(crate) fn walk_guarded_page(
    root: &GuardedPath,
    workspace: &Workspace,
    cursor: Option<&str>,
    options: TraversalOptions,
) -> Result<TraversalPage, ToolError> {
    walk_page_with_before_release(root, workspace, cursor, options, |_, _| Ok(()))
}

fn walk_page_with_before_release(
    root: &GuardedPath,
    workspace: &Workspace,
    cursor: Option<&str>,
    options: TraversalOptions,
    before_release: impl FnOnce(usize, usize) -> Result<(), ToolError>,
) -> Result<TraversalPage, ToolError> {
    walk_page_with_observers(
        root,
        workspace,
        cursor,
        options,
        || Ok(()),
        || Ok(()),
        before_release,
    )
}

fn walk_page_with_observers(
    root: &GuardedPath,
    workspace: &Workspace,
    cursor: Option<&str>,
    options: TraversalOptions,
    after_ignore_capture: impl FnOnce() -> Result<(), ToolError>,
    before_ignore_revalidation: impl FnOnce() -> Result<(), ToolError>,
    before_release: impl FnOnce(usize, usize) -> Result<(), ToolError>,
) -> Result<TraversalPage, ToolError> {
    let requested_root_handle = PathGuard::open_validated_metadata_handle(root)?;
    if !requested_root_handle.metadata()?.is_dir() {
        return Err(ToolError::Message(format!(
            "traversal root `{}` is not a directory",
            root.absolute
        )));
    }
    let requested_root_identity = PathGuard::opened_object_identity(&requested_root_handle)?;
    let ignore_plan_sha256 = ignore_plan_sha256(&workspace.ignore)?;
    let (snapshot_id, mut snapshot, resume_path) = workspace.traversal_registry.checkout(
        &root.absolute,
        cursor,
        options,
        ignore_plan_sha256,
        &requested_root_identity,
    )?;
    let traversal_root = snapshot.root.clone();
    let traversal_guard = PathGuard::require_path(workspace, &traversal_root, AccessKind::Search)?;
    if !PathGuard::same_existing_object_identity(&root.absolute, &traversal_guard.absolute)? {
        return Err(ToolError::Message(
            "traversal root identity changed while its boundary was revalidated".to_string(),
        ));
    }
    let traversal_root_handle = PathGuard::open_validated_metadata_handle(&traversal_guard)?;
    if !traversal_root_handle.metadata()?.is_dir() {
        return Err(ToolError::Message(format!(
            "stored traversal root `{traversal_root}` is not a directory"
        )));
    }
    let traversal_root_identity = PathGuard::opened_object_identity(&traversal_root_handle)?;
    if traversal_root_identity != snapshot.root_identity {
        return Err(ToolError::Message(
            "stored traversal root object changed after cursor admission".to_string(),
        ));
    }
    validate_directory_stamps(&snapshot.directory_stamps, workspace, &traversal_guard)?;
    let mut initial_ignore_state = IgnorePageState::from_snapshot(
        &snapshot.ignore_sources,
        &snapshot.ignore_repository_boundaries,
        workspace.ignore.use_gitignore,
    )?;
    initial_ignore_state.record_ancestors(&traversal_root)?;
    let ignore_state = Arc::new(Mutex::new(initial_ignore_state));
    after_ignore_capture()?;
    let result_limit = options.result_limit.max(1);
    let visit_limit = options.visit_limit.max(1);
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(&traversal_root);
    // Ignore-file semantics are evaluated below from the same captured bytes
    // that own the continuation fingerprint. WalkBuilder must not reopen the
    // path and create a second, racy semantics owner.
    builder.hidden(false);
    builder.ignore(false);
    builder.parents(false);
    builder.git_ignore(false);
    builder.git_global(false);
    builder.git_exclude(false);
    builder.max_depth(options.max_depth);
    builder.sort_by_file_path(compare_traversal_path);
    let filter_root = traversal_root.clone();
    let workspace_root = workspace.root.clone();
    let protected_paths = workspace.protected_paths.clone();
    let ignore_plan = workspace.ignore.clone();
    let resume_filter = resume_path.clone();
    let filter_error = Arc::new(Mutex::new(None));
    let filter_error_sink = Arc::clone(&filter_error);
    let ignore_state_sink = Arc::clone(&ignore_state);
    let include_hidden = options.include_hidden;
    let retained_directories = Arc::new(Mutex::new(Vec::<(GuardedPath, fs::File)>::new()));
    #[cfg(windows)]
    let retained_directory_sink = Arc::clone(&retained_directories);
    let boundary_workspace = workspace.clone();
    let boundary_root = traversal_guard.clone();
    builder.filter_entry(move |entry| {
        let Some(path) = Utf8Path::from_path(entry.path()) else {
            // Preserve fail-closed UTF-8 handling in the main traversal loop.
            return true;
        };
        if compare_traversal_path(path.as_std_path(), filter_root.as_std_path()) == Ordering::Equal
        {
            return true;
        }
        for protected in &protected_paths {
            match PathGuard::security_path_is_within(path, protected) {
                Ok(true) => return false,
                Ok(false) => {}
                Err(error) => {
                    let mut first_error = filter_error_sink
                        .lock()
                        .expect("traversal filter error mutex poisoned");
                    if first_error.is_none() {
                        *first_error = Some(ToolError::from(error));
                    }
                    return false;
                }
            }
        }
        let ignored = ignore_plan.matches_compiled(&ignore, &workspace_root, path)
            || entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir())
                && ignore_plan.matches_compiled(
                    &ignore,
                    &workspace_root,
                    &path.join("__moyai_traversal_descendant__"),
                );
        if ignored {
            return false;
        }
        let is_directory = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir());
        let ignore_decision = ignore_state_sink
            .lock()
            .expect("traversal ignore state mutex poisoned")
            .matched(path, is_directory);
        if ignore_decision == Some(IgnoreDecision::Ignore)
            || ignore_decision.is_none() && !include_hidden && is_hidden_entry(entry)
        {
            return false;
        }
        let admitted_by_resume = resume_filter.as_ref().is_none_or(|resume| {
            resume.starts_with(path)
                || compare_traversal_path(path.as_std_path(), resume.as_std_path())
                    != Ordering::Less
        });
        if !admitted_by_resume {
            return false;
        }
        if is_directory {
            let retained = (|| {
                let guarded = PathGuard::require_descendant(
                    &boundary_workspace,
                    &boundary_root,
                    path,
                )?;
                let handle = PathGuard::open_validated_metadata_handle(&guarded)?;
                if !handle.metadata()?.is_dir() {
                    return Err(crate::error::WorkspaceError::Message(format!(
                        "traversal directory `{path}` changed type while it was validated"
                    )));
                }
                Ok::<_, crate::error::WorkspaceError>((guarded, handle))
            })();
            match retained {
                Ok(retained) => {
                    if let Err(error) = ignore_state_sink
                        .lock()
                        .expect("traversal ignore state mutex poisoned")
                        .record_directory(path)
                    {
                        let mut first_error = filter_error_sink
                            .lock()
                            .expect("traversal filter error mutex poisoned");
                        if first_error.is_none() {
                            *first_error = Some(error);
                        }
                        return false;
                    }
                    #[cfg(windows)]
                    {
                    let mut directories = retained_directory_sink
                        .lock()
                        .expect("retained traversal directory mutex poisoned");
                    if directories.len() >= MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT {
                        let mut first_error = filter_error_sink
                            .lock()
                            .expect("traversal filter error mutex poisoned");
                        if first_error.is_none() {
                            *first_error = Some(ToolError::Message(format!(
                                "traversal page exceeded its retained directory handle limit of {MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT}; narrow the path or max_depth"
                            )));
                        }
                        return false;
                    }
                    directories.push(retained);
                    }
                    #[cfg(not(windows))]
                    drop(retained);
                }
                Err(error) => {
                    let mut first_error = filter_error_sink
                        .lock()
                        .expect("traversal filter error mutex poisoned");
                    if first_error.is_none() {
                        *first_error = Some(ToolError::from(error));
                    }
                    return false;
                }
            }
        }
        true
    });

    let mut entries = Vec::new();
    let mut visited_entries = 0usize;
    let mut continuation = None;
    let mut exhausted = true;
    let mut retained_files = Vec::<(GuardedPath, fs::File)>::new();

    for entry in builder.build() {
        if let Some(error) = filter_error
            .lock()
            .expect("traversal filter error mutex poisoned")
            .take()
        {
            return Err(error);
        }
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir())
        {
            if compare_traversal_path(path.as_std_path(), traversal_root.as_std_path())
                == Ordering::Equal
            {
                record_directory_stamp(
                    &mut snapshot.directory_stamps,
                    &path,
                    &traversal_root_handle,
                )?;
            } else if !snapshot.directory_stamps.contains_key(&path) {
                let guarded = PathGuard::require_descendant(workspace, &traversal_guard, &path)?;
                let handle = PathGuard::open_validated_metadata_handle(&guarded)?;
                record_directory_stamp(&mut snapshot.directory_stamps, &path, &handle)?;
            }
        }
        if resume_path.as_ref().is_some_and(|resume| {
            compare_traversal_path(path.as_std_path(), resume.as_std_path()) == Ordering::Less
        }) {
            continue;
        }
        if visited_entries >= visit_limit {
            continuation = Some(path);
            exhausted = false;
            break;
        }
        visited_entries = visited_entries.saturating_add(1);
        if compare_traversal_path(path.as_std_path(), traversal_root.as_std_path())
            == Ordering::Equal
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
        let relative_path = PathGuard::relative_path_from_root(&path, &traversal_root)
            .ok_or_else(|| {
                ToolError::Message(format!(
                    "traversal entry `{path}` could not be projected relative to root `{traversal_root}`"
                ))
            })?;
        let size_bytes = if is_file {
            let guarded = PathGuard::require_descendant(workspace, &traversal_guard, &path)?;
            let handle = PathGuard::open_validated_metadata_handle(&guarded)?;
            let metadata = handle.metadata()?;
            if !metadata.is_file() {
                return Err(ToolError::Message(format!(
                    "traversal file `{path}` changed type while it was validated"
                )));
            }
            let size_bytes = metadata.len();
            #[cfg(windows)]
            retained_files.push((guarded, handle));
            #[cfg(not(windows))]
            drop((guarded, handle));
            Some(size_bytes)
        } else {
            None
        };
        entries.push(TraversalEntry {
            depth: relative_path.components().count(),
            relative_path,
            path,
            is_directory,
            size_bytes,
            cursor: String::new(),
        });
    }

    if let Some(error) = filter_error
        .lock()
        .expect("traversal filter error mutex poisoned")
        .take()
    {
        return Err(error);
    }

    before_ignore_revalidation()?;
    validate_directory_stamps(&snapshot.directory_stamps, workspace, &traversal_guard)?;
    {
        let state = ignore_state
            .lock()
            .expect("traversal ignore state mutex poisoned");
        state.validate()?;
        state.write_snapshot(&mut snapshot);
    }
    PathGuard::validate_open_file(root, &requested_root_handle)?;
    PathGuard::validate_open_file(&traversal_guard, &traversal_root_handle)?;
    {
        let directories = retained_directories
            .lock()
            .expect("retained traversal directory mutex poisoned");
        for (guarded, handle) in directories.iter() {
            PathGuard::validate_open_file(guarded, handle)?;
        }
    }
    for (guarded, handle) in &retained_files {
        PathGuard::validate_open_file(guarded, handle)?;
    }
    let retained_directory_count = retained_directories
        .lock()
        .expect("retained traversal directory mutex poisoned")
        .len();
    before_release(retained_directory_count, retained_files.len())?;
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
        ignore_plan_sha256: [u8; 32],
        root_identity: &ExistingObjectIdentity,
    ) -> Result<(String, TraversalSnapshot, Option<Utf8PathBuf>), ToolError> {
        let cursor = cursor.map(str::trim).filter(|value| !value.is_empty());
        let Some(cursor) = cursor else {
            let snapshot_id = ulid::Ulid::new().to_string();
            return Ok((
                snapshot_id,
                TraversalSnapshot {
                    root: root.to_path_buf(),
                    root_identity: root_identity.clone(),
                    options,
                    generation: 1,
                    directory_stamps: BTreeMap::new(),
                    ignore_plan_sha256,
                    ignore_sources: BTreeMap::new(),
                    ignore_repository_boundaries: BTreeMap::new(),
                    ignore_source_bytes: 0,
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
        if &stored.root_identity != root_identity {
            return Err(ToolError::Message(
                "traversal cursor belongs to a different root".to_string(),
            ));
        }
        if stored.options != options {
            return Err(ToolError::Message(
                "traversal cursor options do not match the original traversal".to_string(),
            ));
        }
        if stored.ignore_plan_sha256 != ignore_plan_sha256 {
            return Err(ToolError::Message(
                "traversal cursor ignore plan does not match the original traversal".to_string(),
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
        if snapshot.ignore_sources.len() > MAX_TRAVERSAL_IGNORE_SOURCES_PER_SNAPSHOT
            || snapshot.ignore_repository_boundaries.len()
                > MAX_TRAVERSAL_IGNORE_SOURCES_PER_SNAPSHOT
            || snapshot.ignore_source_bytes > MAX_IGNORE_SOURCE_BYTES_PER_SNAPSHOT
        {
            return Err(ToolError::Message(
                "traversal snapshot exceeded its bounded ignore-source fence; narrow the path or max_depth"
                    .to_string(),
            ));
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

    pub(crate) fn register_one_shot_continuation(
        &self,
        domain: &str,
        payload: Vec<u8>,
    ) -> Result<String, ToolError> {
        if domain.trim().is_empty() || payload.is_empty() {
            return Err(ToolError::Message(
                "one-shot continuation domain and payload must not be empty".to_string(),
            ));
        }
        if payload.len() > MAX_ONE_SHOT_CONTINUATION_BYTES {
            return Err(ToolError::Message(format!(
                "one-shot continuation payload exceeded the {MAX_ONE_SHOT_CONTINUATION_BYTES} byte limit"
            )));
        }
        let mut registry = self
            .inner
            .lock()
            .expect("traversal registry mutex poisoned");
        registry.prune_expired();
        registry.make_room_for_one_shot();
        for _ in 0..4 {
            let token = ulid::Ulid::new().to_string();
            if registry.one_shot_continuations.contains_key(&token) {
                continue;
            }
            registry.one_shot_continuations.insert(
                token.clone(),
                OneShotContinuation {
                    domain: domain.to_string(),
                    payload,
                    last_used: Instant::now(),
                },
            );
            return Ok(token);
        }
        Err(ToolError::Message(
            "failed to allocate a unique one-shot continuation token".to_string(),
        ))
    }

    pub(crate) fn consume_one_shot_continuation(
        &self,
        domain: &str,
        token: &str,
    ) -> Result<Vec<u8>, ToolError> {
        let mut registry = self
            .inner
            .lock()
            .expect("traversal registry mutex poisoned");
        registry.prune_expired();
        let continuation = registry
            .one_shot_continuations
            .remove(token)
            .ok_or_else(|| {
                ToolError::Message(
                    "continuation token expired, was already consumed, or is invalid".to_string(),
                )
            })?;
        if continuation.domain != domain {
            return Err(ToolError::Message(
                "continuation token belongs to a different operation".to_string(),
            ));
        }
        Ok(continuation.payload)
    }
}

impl TraversalRegistryState {
    fn prune_expired(&mut self) {
        self.snapshots
            .retain(|_, snapshot| snapshot.last_used.elapsed() <= TRAVERSAL_SNAPSHOT_TTL);
        self.one_shot_continuations
            .retain(|_, continuation| continuation.last_used.elapsed() <= TRAVERSAL_SNAPSHOT_TTL);
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

    fn make_room_for_one_shot(&mut self) {
        while self.one_shot_continuations.len() >= MAX_ACTIVE_ONE_SHOT_CONTINUATIONS {
            let Some(oldest) = self
                .one_shot_continuations
                .iter()
                .min_by_key(|(_, continuation)| continuation.last_used)
                .map(|(token, _)| token.clone())
            else {
                break;
            };
            self.one_shot_continuations.remove(&oldest);
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
    if !PathGuard::security_path_is_within(path, &snapshot.root)?
        || PathGuard::same_existing_object_identity(path, &snapshot.root)?
    {
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
    handle: &fs::File,
) -> Result<(), ToolError> {
    if stamps.contains_key(path) {
        return Ok(());
    }
    if stamps.len() >= MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT {
        return Err(ToolError::Message(format!(
            "traversal snapshot exceeded its directory fence limit of {MAX_TRAVERSAL_DIRECTORIES_PER_SNAPSHOT}; narrow the path or max_depth"
        )));
    }
    stamps.insert(path.to_path_buf(), directory_stamp(path, handle)?);
    Ok(())
}

fn validate_directory_stamps(
    stamps: &BTreeMap<Utf8PathBuf, DirectoryStamp>,
    workspace: &Workspace,
    traversal_root: &GuardedPath,
) -> Result<(), ToolError> {
    for (path, expected) in stamps {
        let current = (|| {
            let guarded = PathGuard::require_descendant(workspace, traversal_root, path)?;
            let handle = PathGuard::open_validated_metadata_handle(&guarded)?;
            directory_stamp(path, &handle)
        })()
        .map_err(|_| {
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

fn directory_stamp(path: &Utf8Path, handle: &fs::File) -> Result<DirectoryStamp, ToolError> {
    let metadata = handle.metadata().map_err(|error| {
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
        object_identity: PathGuard::opened_object_identity(handle)?,
        modified_nanos,
        created_nanos,
        metadata_len: metadata.len(),
    })
}

fn ignore_plan_sha256(plan: &crate::workspace::IgnorePlan) -> Result<[u8; 32], ToolError> {
    let encoded = serde_json::to_vec(plan).map_err(|error| {
        ToolError::Message(format!(
            "failed to fingerprint the traversal ignore plan: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"moyai-traversal-ignore-plan-v1");
    hasher.update((encoded.len() as u64).to_le_bytes());
    hasher.update(encoded);
    Ok(hasher.finalize().into())
}

impl IgnorePageState {
    fn from_snapshot(
        expected_sources: &BTreeMap<Utf8PathBuf, IgnoreSourceStamp>,
        expected_boundaries: &BTreeMap<Utf8PathBuf, bool>,
        use_gitignore: bool,
    ) -> Result<Self, ToolError> {
        let mut state = Self {
            sources: BTreeMap::new(),
            repository_boundaries: BTreeMap::new(),
            source_bytes: 0,
            use_gitignore,
        };
        for (path, expected) in expected_sources {
            let captured = capture_ignore_source(path).map_err(|error| {
                ToolError::Message(format!(
                    "traversal ignore source `{path}` is no longer readable ({error}); restart the traversal"
                ))
            })?;
            if captured.stamp != *expected {
                return Err(ignore_semantics_changed(path));
            }
            state.insert_source(path, captured)?;
        }
        for (directory, expected) in expected_boundaries {
            let current = has_repository_boundary(directory);
            if current != *expected {
                return Err(ToolError::Message(format!(
                    "traversal ignore repository boundary changed at `{directory}`; restart the traversal to avoid omitted or duplicated paths"
                )));
            }
            state
                .repository_boundaries
                .insert(directory.clone(), current);
        }
        Ok(state)
    }

    fn record_ancestors(&mut self, root: &Utf8Path) -> Result<(), ToolError> {
        let mut current = Some(root);
        for _ in 0..MAX_IGNORE_ANCESTOR_DIRECTORIES {
            let Some(directory) = current else {
                return Ok(());
            };
            self.record_directory(directory)?;
            current = directory.parent().filter(|parent| *parent != directory);
        }
        if current.is_some() {
            return Err(ToolError::Message(format!(
                "traversal root `{root}` exceeded the bounded ignore-ancestor depth of {MAX_IGNORE_ANCESTOR_DIRECTORIES}"
            )));
        }
        Ok(())
    }

    fn record_directory(&mut self, directory: &Utf8Path) -> Result<(), ToolError> {
        self.record_source(&directory.join(".ignore"))?;
        if self.use_gitignore {
            self.record_source(&directory.join(".gitignore"))?;
            self.repository_boundaries
                .entry(directory.to_path_buf())
                .or_insert_with(|| has_repository_boundary(directory));
        }
        Ok(())
    }

    fn record_source(&mut self, path: &Utf8Path) -> Result<(), ToolError> {
        if self.sources.contains_key(path) {
            return Ok(());
        }
        let captured = capture_ignore_source(path)?;
        self.insert_source(path, captured)
    }

    fn insert_source(
        &mut self,
        path: &Utf8Path,
        captured: CapturedIgnoreSource,
    ) -> Result<(), ToolError> {
        if self.sources.len() >= MAX_TRAVERSAL_IGNORE_SOURCES_PER_SNAPSHOT {
            return Err(ToolError::Message(format!(
                "traversal snapshot exceeded its ignore-source limit of {MAX_TRAVERSAL_IGNORE_SOURCES_PER_SNAPSHOT}; narrow the path or max_depth"
            )));
        }
        let next_bytes = self.source_bytes.saturating_add(captured.stamp.content_len);
        if next_bytes > MAX_IGNORE_SOURCE_BYTES_PER_SNAPSHOT {
            return Err(ToolError::Message(format!(
                "traversal snapshot ignore sources exceeded the {MAX_IGNORE_SOURCE_BYTES_PER_SNAPSHOT} byte fingerprint limit; narrow the path or max_depth"
            )));
        }
        self.sources.insert(path.to_path_buf(), captured);
        self.source_bytes = next_bytes;
        Ok(())
    }

    fn matched(&self, path: &Utf8Path, is_directory: bool) -> Option<IgnoreDecision> {
        let mut current = path.parent();
        let mut any_git = false;
        while let Some(directory) = current {
            any_git |= self
                .repository_boundaries
                .get(directory)
                .copied()
                .unwrap_or(false);
            current = directory.parent().filter(|parent| *parent != directory);
        }

        let mut ignore_match = None;
        let mut gitignore_match = None;
        let mut saw_repository_boundary = false;
        let mut current = path.parent();
        while let Some(directory) = current {
            if ignore_match.is_none() {
                ignore_match = self.source_match(&directory.join(".ignore"), path, is_directory);
            }
            if self.use_gitignore
                && any_git
                && !saw_repository_boundary
                && gitignore_match.is_none()
            {
                gitignore_match =
                    self.source_match(&directory.join(".gitignore"), path, is_directory);
            }
            saw_repository_boundary |= self
                .repository_boundaries
                .get(directory)
                .copied()
                .unwrap_or(false);
            current = directory.parent().filter(|parent| *parent != directory);
        }
        ignore_match.or(gitignore_match)
    }

    fn source_match(
        &self,
        source: &Utf8Path,
        path: &Utf8Path,
        is_directory: bool,
    ) -> Option<IgnoreDecision> {
        let matcher = self.sources.get(source)?.matcher.as_ref()?;
        let matched = matcher.matched(path, is_directory);
        if matched.is_ignore() {
            Some(IgnoreDecision::Ignore)
        } else if matched.is_whitelist() {
            Some(IgnoreDecision::Whitelist)
        } else {
            None
        }
    }

    fn validate(&self) -> Result<(), ToolError> {
        for (path, expected) in &self.sources {
            let current = capture_ignore_source(path).map_err(|error| {
                ToolError::Message(format!(
                    "traversal ignore source `{path}` is no longer readable ({error}); restart the traversal"
                ))
            })?;
            if current.stamp != expected.stamp {
                return Err(ignore_semantics_changed(path));
            }
        }
        for (directory, expected) in &self.repository_boundaries {
            if has_repository_boundary(directory) != *expected {
                return Err(ToolError::Message(format!(
                    "traversal ignore repository boundary changed at `{directory}`; restart the traversal to avoid omitted or duplicated paths"
                )));
            }
        }
        Ok(())
    }

    fn write_snapshot(&self, snapshot: &mut TraversalSnapshot) {
        snapshot.ignore_sources = self
            .sources
            .iter()
            .map(|(path, source)| (path.clone(), source.stamp))
            .collect();
        snapshot.ignore_repository_boundaries = self.repository_boundaries.clone();
        snapshot.ignore_source_bytes = self.source_bytes;
    }
}

fn ignore_semantics_changed(path: &Utf8Path) -> ToolError {
    ToolError::Message(format!(
        "traversal ignore semantics changed at `{path}`; restart the traversal to avoid omitted or duplicated paths"
    ))
}

fn has_repository_boundary(directory: &Utf8Path) -> bool {
    directory.join(".git").exists() || directory.join(".jj").exists()
}

fn capture_ignore_source(path: &Utf8Path) -> Result<CapturedIgnoreSource, ToolError> {
    capture_ignore_source_with_observers(path, || Ok(()), |_| Ok(()))
}

fn capture_ignore_source_with_observers(
    path: &Utf8Path,
    before_open: impl FnOnce() -> Result<(), ToolError>,
    after_open: impl FnOnce(&fs::File) -> Result<(), ToolError>,
) -> Result<CapturedIgnoreSource, ToolError> {
    let link_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CapturedIgnoreSource {
                stamp: IgnoreSourceStamp {
                    kind: IgnoreSourceKind::Missing,
                    content_len: 0,
                    content_sha256: [0; 32],
                },
                matcher: None,
            });
        }
        Err(error) => {
            return Err(ToolError::Message(format!(
                "failed to inspect traversal ignore source `{path}`: {error}"
            )));
        }
    };
    if !link_metadata.is_file() && !link_metadata.file_type().is_symlink() {
        return Ok(CapturedIgnoreSource {
            stamp: IgnoreSourceStamp {
                kind: IgnoreSourceKind::Other,
                content_len: 0,
                content_sha256: [0; 32],
            },
            matcher: None,
        });
    }

    before_open()?;
    let guarded = PathGuard::trusted_exact_path(path)?;
    let mut file = open_ignore_source_file(path).map_err(|error| {
        ToolError::Message(format!(
            "failed to open traversal ignore source `{path}`: {error}"
        ))
    })?;
    PathGuard::validate_open_file(&guarded, &file)?;
    after_open(&file)?;
    let target_metadata = file.metadata().map_err(|error| {
        ToolError::Message(format!(
            "failed to inspect opened traversal ignore source `{path}`: {error}"
        ))
    })?;
    if !target_metadata.is_file() {
        return Ok(CapturedIgnoreSource {
            stamp: IgnoreSourceStamp {
                kind: IgnoreSourceKind::Other,
                content_len: 0,
                content_sha256: [0; 32],
            },
            matcher: None,
        });
    }

    let mut bytes = Vec::with_capacity(
        usize::try_from(target_metadata.len().min(MAX_IGNORE_SOURCE_BYTES)).unwrap_or(0),
    );
    let mut limited = (&mut file).take(MAX_IGNORE_SOURCE_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        ToolError::Message(format!(
            "failed to read traversal ignore source `{path}`: {error}"
        ))
    })?;
    if bytes.len() as u64 > MAX_IGNORE_SOURCE_BYTES {
        return Err(ToolError::Message(format!(
            "traversal ignore source `{path}` exceeded the {MAX_IGNORE_SOURCE_BYTES} byte limit while it was read"
        )));
    }
    let matcher = compile_ignore_matcher(path, &bytes);
    let content_len = bytes.len() as u64;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(CapturedIgnoreSource {
        stamp: IgnoreSourceStamp {
            kind: if link_metadata.file_type().is_symlink() {
                IgnoreSourceKind::SymlinkFile
            } else {
                IgnoreSourceKind::RegularFile
            },
            content_len,
            content_sha256: hasher.finalize().into(),
        },
        matcher: Some(matcher),
    })
}

fn compile_ignore_matcher(path: &Utf8Path, bytes: &[u8]) -> Gitignore {
    let root = path.parent().unwrap_or(Utf8Path::new("/"));
    let mut builder = GitignoreBuilder::new(root);
    for (index, line) in std::io::Cursor::new(bytes).lines().enumerate() {
        let Ok(line) = line else {
            break;
        };
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            &line
        };
        // WalkBuilder keeps every valid pattern from a partially-invalid
        // ignore file. Its parse error is attached to the directory entry,
        // which this traversal has historically not surfaced, so preserve
        // that behavior while compiling from fixed bytes.
        let _ = builder.add_line(Some(path.as_std_path().to_path_buf()), line);
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

#[cfg(unix)]
fn open_ignore_source_file(path: &Utf8Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    options.open(path)
}

#[cfg(windows)]
fn open_ignore_source_file(path: &Utf8Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ};

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_ignore_source_file(path: &Utf8Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

fn is_hidden_entry(entry: &ignore::DirEntry) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN;

        if entry
            .metadata()
            .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
        {
            return true;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        return entry
            .path()
            .file_name()
            .is_some_and(|name| name.as_bytes().first() == Some(&b'.'));
    }
    #[cfg(not(unix))]
    entry
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions};
    use std::time::Duration;

    use camino::Utf8PathBuf;

    use crate::config::ResolvedConfig;
    use crate::workspace::{AccessKind, PathGuard, WorkspaceDiscovery};

    use super::{
        MAX_ACTIVE_ONE_SHOT_CONTINUATIONS, TraversalOptions, TraversalRegistry,
        capture_ignore_source_with_observers, compare_traversal_path, walk_page,
        walk_page_with_before_release, walk_page_with_observers,
    };

    #[cfg(windows)]
    fn assert_sharing_violation(error: &std::io::Error, operation: &str) {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32),
            "{operation} must fail while traversal retains a no-delete-share handle: {error}"
        );
    }

    #[cfg(unix)]
    fn link_directory(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("create directory symlink");
    }

    #[cfg(windows)]
    fn enable_case_sensitive_directory(path: &camino::Utf8Path) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileCaseSensitiveInfo,
            SetFileInformationByHandle,
        };
        use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        let directory = options.open(path).expect("open case-sensitive directory");
        let info = FILE_CASE_SENSITIVE_INFO {
            Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR,
        };
        let result = unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle() as HANDLE,
                FileCaseSensitiveInfo,
                (&info as *const FILE_CASE_SENSITIVE_INFO).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        assert_ne!(
            result,
            0,
            "enable per-directory case sensitivity: {}",
            std::io::Error::last_os_error()
        );
    }

    fn rewrite_same_size_preserving_modified_time(path: &camino::Utf8Path, replacement: &[u8]) {
        let before = std::fs::metadata(path).expect("metadata before rewrite");
        let modified = before.modified().expect("modified time before rewrite");
        assert_eq!(before.len(), replacement.len() as u64);
        std::fs::write(path, replacement).expect("rewrite fixture in place");
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open rewritten fixture")
            .set_times(FileTimes::new().set_modified(modified))
            .expect("restore modified time");
    }

    fn copy_directory_stamp_metadata(
        target: &mut super::DirectoryStamp,
        current: &super::DirectoryStamp,
    ) {
        target.modified_nanos = current.modified_nanos;
        target.created_nanos = current.created_nanos;
        target.metadata_len = current.metadata_len;
    }

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

    #[cfg(windows)]
    #[test]
    fn traversal_retains_root_and_directory_namespace_handles_until_page_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 workspace root");
        let root = workspace_root.join("search-root");
        let child = root.join("child");
        std::fs::create_dir_all(&child).expect("child directory");
        std::fs::write(child.join("inside.txt"), "inside").expect("inside fixture");
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&workspace_root, &ResolvedConfig::default())
                .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard traversal root");
        let parked_root = workspace_root.join("parked-root");
        let parked_child = root.join("parked-child");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: true,
            result_limit: 16,
            visit_limit: 64,
        };

        let page = walk_page_with_before_release(
            &guarded,
            &workspace,
            None,
            options,
            |retained_directories, retained_files| {
                assert!(retained_directories >= 1);
                assert!(retained_files >= 1);
                let root_error = std::fs::rename(&root, &parked_root)
                    .expect_err("retained root handle must block namespace replacement");
                assert_sharing_violation(&root_error, "traversal root rename");
                let child_error = std::fs::rename(&child, &parked_child)
                    .expect_err("retained child handle must block namespace replacement");
                assert_sharing_violation(&child_error, "traversal child rename");
                Ok(())
            },
        )
        .expect("bounded traversal page");

        assert!(
            page.entries
                .iter()
                .all(|entry| entry.path.starts_with(&root))
        );
        assert!(
            page.entries
                .iter()
                .any(|entry| entry.relative_path == Utf8PathBuf::from("child").join("inside.txt"))
        );

        std::fs::rename(&child, &parked_child)
            .expect("child rename after traversal handle release");
        std::fs::rename(&parked_child, &child).expect("restore child directory");
        std::fs::rename(&root, &parked_root).expect("root rename after traversal handle release");
        std::fs::rename(&parked_root, &root).expect("restore traversal root");
    }

    #[cfg(windows)]
    #[test]
    fn continuation_retains_only_resume_admitted_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a", "b", "c"] {
            let directory = root.join(name);
            std::fs::create_dir(&directory).expect("fixture directory");
            std::fs::write(directory.join("entry.txt"), name).expect("fixture file");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: false,
            include_directories: true,
            result_limit: 1,
            visit_limit: 64,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        assert_eq!(first.entries[0].relative_path.as_str(), "a");
        let cursor = first.continuation.expect("resume at the next directory");
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard traversal root");

        let second = walk_page_with_before_release(
            &guarded,
            &workspace,
            Some(&cursor),
            options,
            |retained_directories, retained_files| {
                assert_eq!(
                    retained_directories, 2,
                    "only the returned directory and the next continuation candidate are retained"
                );
                assert_eq!(retained_files, 0);
                Ok(())
            },
        )
        .expect("second page");
        assert_eq!(second.entries[0].relative_path.as_str(), "b");
    }

    #[cfg(unix)]
    #[test]
    fn continuation_accepts_a_symlink_alias_with_the_snapshot_root_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 workspace root");
        let root = workspace_root.join("real-root");
        std::fs::create_dir(&root).expect("real traversal root");
        std::fs::write(root.join("a.txt"), "a").expect("first fixture");
        std::fs::write(root.join("b.txt"), "b").expect("second fixture");
        let alias = workspace_root.join("alias-root");
        link_directory(&root, &alias);
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&workspace_root, &ResolvedConfig::default())
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
        assert_eq!(first.entries[0].relative_path.as_str(), "a.txt");
        let cursor = first.continuation.expect("continuation");
        let second = walk_page(&alias, &workspace, Some(&cursor), options)
            .expect("resume through a symlink alias of the same root object");

        assert_eq!(second.entries[0].relative_path.as_str(), "b.txt");
        assert!(second.entries[0].path.starts_with(&root));
    }

    #[cfg(unix)]
    #[test]
    fn unix_traversal_does_not_retain_a_page_of_file_descriptors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for index in 0..1_030 {
            std::fs::write(root.join(format!("{index:04}.txt")), []).expect("fixture file");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard traversal root");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1_030,
            visit_limit: 2_048,
        };

        let page = walk_page_with_before_release(
            &guarded,
            &workspace,
            None,
            options,
            |retained_directories, retained_files| {
                assert_eq!(retained_directories, 0);
                assert_eq!(retained_files, 0);
                Ok(())
            },
        )
        .expect("large page without retained file descriptors");

        assert_eq!(page.entries.len(), 1_030);
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
    fn continuation_rejects_a_replacement_root_with_colliding_directory_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 parent");
        let root = parent.join("workspace");
        std::fs::create_dir(&root).expect("workspace root");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), name).expect("fixture file");
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

        let parked = parent.join("parked-workspace");
        std::fs::rename(&root, &parked).expect("park original root");
        std::fs::create_dir(&root).expect("replacement root");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), name).expect("replacement fixture");
        }
        let current_guard = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard replacement root");
        let current_handle = PathGuard::open_validated_metadata_handle(&current_guard)
            .expect("open replacement root");
        let current_stamp =
            super::directory_stamp(&root, &current_handle).expect("replacement root stamp");
        let payload = super::decode_cursor_payload(&cursor).expect("decode traversal cursor");
        {
            let mut registry = workspace
                .traversal_registry
                .inner
                .lock()
                .expect("traversal registry mutex poisoned");
            let snapshot = registry
                .snapshots
                .get_mut(&payload.snapshot_id)
                .expect("stored traversal snapshot");
            let stored = snapshot
                .directory_stamps
                .get_mut(&root)
                .expect("stored root stamp");
            copy_directory_stamp_metadata(stored, &current_stamp);
        }

        let error = walk_page(&root, &workspace, Some(&cursor), options)
            .expect_err("replacement root object must invalidate the old cursor");
        assert!(error.to_string().contains("different root"));
    }

    #[test]
    fn continuation_rejects_a_replacement_descendant_with_colliding_directory_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let child = root.join("child");
        std::fs::create_dir(&child).expect("child directory");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(child.join(name), name).expect("fixture file");
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

        let parked = root.join("parked-child");
        std::fs::rename(&child, &parked).expect("park original child");
        std::fs::create_dir(&child).expect("replacement child");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(child.join(name), name).expect("replacement fixture");
        }
        let root_guard = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard traversal root");
        let root_handle =
            PathGuard::open_validated_metadata_handle(&root_guard).expect("open traversal root");
        let current_root_stamp =
            super::directory_stamp(&root, &root_handle).expect("current root stamp");
        let child_guard = PathGuard::require_path(&workspace, &child, AccessKind::Search)
            .expect("guard replacement child");
        let child_handle = PathGuard::open_validated_metadata_handle(&child_guard)
            .expect("open replacement child");
        let current_child_stamp =
            super::directory_stamp(&child, &child_handle).expect("replacement child stamp");
        let payload = super::decode_cursor_payload(&cursor).expect("decode traversal cursor");
        {
            let mut registry = workspace
                .traversal_registry
                .inner
                .lock()
                .expect("traversal registry mutex poisoned");
            let snapshot = registry
                .snapshots
                .get_mut(&payload.snapshot_id)
                .expect("stored traversal snapshot");
            copy_directory_stamp_metadata(
                snapshot
                    .directory_stamps
                    .get_mut(&root)
                    .expect("stored root stamp"),
                &current_root_stamp,
            );
            copy_directory_stamp_metadata(
                snapshot
                    .directory_stamps
                    .get_mut(&child)
                    .expect("stored child stamp"),
                &current_child_stamp,
            );
        }

        let error = walk_page(&root, &workspace, Some(&cursor), options)
            .expect_err("replacement child object must invalidate the old cursor");
        assert!(error.to_string().contains("snapshot changed"));
        assert!(error.to_string().contains("child"));
    }

    #[test]
    fn continuation_rejects_in_place_ignore_source_changes_before_resume_candidate() {
        for ignore_name in [".gitignore", ".ignore"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
            std::fs::write(root.join("a.txt"), "a").expect("first fixture");
            std::fs::write(root.join("b.txt"), "b").expect("second fixture");
            std::fs::write(root.join("c.txt"), "c").expect("third fixture");
            let ignore_path = root.join(ignore_name);
            std::fs::write(&ignore_path, b"b.txt\n").expect("initial ignore source");
            let workspace =
                WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
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
            assert_eq!(first.entries[0].relative_path, "a.txt");
            let cursor = first.continuation.expect("continuation");

            rewrite_same_size_preserving_modified_time(&ignore_path, b"c.txt\n");

            let error = walk_page(&root, &workspace, Some(&cursor), options)
                .expect_err("ignore semantics changed without a directory stamp change");
            assert!(error.to_string().contains("ignore semantics changed"));
        }
    }

    #[test]
    fn ignore_fingerprint_and_walk_use_the_same_fixed_bytes_across_an_aba() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).expect("fixture");
        }
        let ignore_path = root.join(".ignore");
        std::fs::write(&ignore_path, b"a.txt\n").expect("initial ignore source A");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard traversal root");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 16,
        };

        let first = walk_page_with_observers(
            &guarded,
            &workspace,
            None,
            options,
            || {
                rewrite_same_size_preserving_modified_time(&ignore_path, b"b.txt\n");
                Ok(())
            },
            || {
                rewrite_same_size_preserving_modified_time(&ignore_path, b"a.txt\n");
                Ok(())
            },
            |_, _| Ok(()),
        )
        .expect("walk fixed to source A despite the path-level ABA");

        assert_eq!(first.entries[0].relative_path, "b.txt");
        let second = walk_page(&root, &workspace, first.continuation.as_deref(), options)
            .expect("resume with the same fixed ignore semantics");
        assert_eq!(second.entries[0].relative_path, "c.txt");
        assert!(second.continuation.is_none());
    }

    #[test]
    fn fixed_ignore_matching_preserves_category_ancestor_and_hidden_whitelist_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 parent");
        let root = parent.join("search");
        let nested = root.join("nested");
        std::fs::create_dir_all(root.join(".git")).expect("repository boundary");
        std::fs::create_dir(&nested).expect("nested directory");
        std::fs::write(parent.join(".ignore"), "ancestor-hidden.txt\n")
            .expect("ancestor ignore source");
        std::fs::write(
            root.join(".ignore"),
            "nested/item.txt\n.*\n!.revealed\n!category.txt\n",
        )
        .expect("root ignore source");
        std::fs::write(root.join(".gitignore"), "category.txt\n").expect("gitignore source");
        std::fs::write(nested.join(".ignore"), "!item.txt\n").expect("nested ignore source");
        for name in [
            "ancestor-hidden.txt",
            "category.txt",
            ".revealed",
            ".concealed",
            "visible.txt",
        ] {
            std::fs::write(root.join(name), name).expect("root fixture");
        }
        std::fs::write(nested.join("item.txt"), "nested").expect("nested fixture");
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&parent, &ResolvedConfig::default())
                .expect("workspace");

        let page = walk_page(
            &root,
            &workspace,
            None,
            TraversalOptions {
                include_hidden: false,
                max_depth: None,
                include_files: true,
                include_directories: false,
                result_limit: 16,
                visit_limit: 64,
            },
        )
        .expect("fixed ignore traversal");
        let mut paths = page
            .entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect::<Vec<_>>();
        paths.sort();

        assert_eq!(
            paths,
            vec![
                Utf8PathBuf::from(".revealed"),
                Utf8PathBuf::from("category.txt"),
                Utf8PathBuf::from("nested").join("item.txt"),
                Utf8PathBuf::from("visible.txt"),
            ]
        );
    }

    #[test]
    fn partially_invalid_ignore_source_keeps_its_valid_patterns() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut invalid_probe = super::GitignoreBuilder::new(&root);
        assert!(
            invalid_probe.add_line(None, "[z-a]").is_err(),
            "fixture must remain an invalid glob"
        );
        std::fs::write(root.join(".ignore"), "ignored.txt\n[z-a]\n")
            .expect("partially-invalid ignore source");
        std::fs::write(root.join("ignored.txt"), "ignored").expect("ignored fixture");
        std::fs::write(root.join("visible.txt"), "visible").expect("visible fixture");
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
                include_directories: false,
                result_limit: 8,
                visit_limit: 32,
            },
        )
        .expect("valid patterns remain active despite a later invalid glob");

        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["visible.txt"]
        );
    }

    #[test]
    fn gitignore_requires_a_repository_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        std::fs::write(root.join(".gitignore"), "blocked.txt\n").expect("gitignore source");
        std::fs::write(root.join("blocked.txt"), "blocked").expect("blocked fixture");
        std::fs::write(root.join("visible.txt"), "visible").expect("visible fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 8,
            visit_limit: 32,
        };

        let without_repository = walk_page(&root, &workspace, None, options)
            .expect("gitignore is inactive without a repository boundary");
        assert_eq!(
            without_repository
                .entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["blocked.txt", "visible.txt"]
        );

        std::fs::create_dir(root.join(".git")).expect("repository boundary");
        let with_repository = walk_page(&root, &workspace, None, options)
            .expect("gitignore is active inside a repository");
        assert_eq!(
            with_repository
                .entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["visible.txt"]
        );
    }

    #[test]
    fn gitignore_does_not_cross_the_nearest_repository_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outer = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 outer");
        let root = outer.join("search");
        std::fs::create_dir_all(outer.join(".git")).expect("outer repository boundary");
        std::fs::create_dir(&root).expect("traversal root");
        std::fs::write(outer.join(".gitignore"), "blocked.txt\n").expect("outer gitignore source");
        std::fs::write(root.join("blocked.txt"), "blocked").expect("blocked fixture");
        std::fs::write(root.join("visible.txt"), "visible").expect("visible fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&outer, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 8,
            visit_limit: 32,
        };

        let inherited = walk_page(&root, &workspace, None, options)
            .expect("outer repository gitignore applies before an inner boundary");
        assert_eq!(
            inherited
                .entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["visible.txt"]
        );

        std::fs::create_dir(root.join(".git")).expect("inner repository boundary");
        let isolated = walk_page(&root, &workspace, None, options)
            .expect("inner repository boundary stops the outer gitignore");
        assert_eq!(
            isolated
                .entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["blocked.txt", "visible.txt"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn ignore_source_capture_pins_content_and_namespace_on_windows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let source = root.join(".ignore");
        let parked = root.join("parked-ignore");
        std::fs::write(&source, "ignored.txt\n").expect("ignore fixture");

        capture_ignore_source_with_observers(
            &source,
            || Ok(()),
            |_| {
                let write_error = OpenOptions::new()
                    .write(true)
                    .open(&source)
                    .expect_err("capture handle must reject a concurrent writer");
                assert_sharing_violation(&write_error, "ignore source write-open");
                let rename_error = std::fs::rename(&source, &parked)
                    .expect_err("capture handle must reject namespace replacement");
                assert_sharing_violation(&rename_error, "ignore source rename");
                Ok(())
            },
        )
        .expect("stable ignore source capture");

        std::fs::rename(&source, &parked).expect("rename after capture handle release");
        std::fs::rename(&parked, &source).expect("restore ignore source");
    }

    #[cfg(unix)]
    #[test]
    fn ignore_source_capture_does_not_block_when_regular_file_becomes_fifo() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::sync::mpsc;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let source = root.join(".ignore");
        let parked = root.join("parked-ignore");
        std::fs::write(&source, "ignored.txt\n").expect("regular ignore fixture");
        let worker_source = source.clone();
        let worker_parked = parked.clone();
        let (sender, receiver) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            let result = capture_ignore_source_with_observers(
                &worker_source,
                || {
                    std::fs::rename(&worker_source, &worker_parked)
                        .expect("park regular source before open");
                    let fifo = CString::new(worker_source.as_std_path().as_os_str().as_bytes())
                        .expect("FIFO path without NUL");
                    let result = unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
                    assert_eq!(
                        result,
                        0,
                        "create FIFO replacement: {}",
                        std::io::Error::last_os_error()
                    );
                    Ok(())
                },
                |_| Ok(()),
            )
            .map(|captured| captured.stamp.kind);
            sender.send(result).expect("send capture result");
        });

        let result = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result,
            Err(error) => {
                let mut rescue = OpenOptions::new();
                rescue.read(true).write(true).custom_flags(libc::O_NONBLOCK);
                if let Ok(mut fifo) = rescue.open(&source) {
                    let _ = std::io::Write::write_all(&mut fifo, b"\n");
                }
                let _ = worker.join();
                panic!("ignore source capture blocked on a FIFO replacement: {error}");
            }
        };
        worker.join().expect("capture worker");

        assert_eq!(
            result.expect("FIFO replacement is classified without reading"),
            super::IgnoreSourceKind::Other
        );
    }

    #[test]
    fn continuation_rejects_a_changed_custom_ignore_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), name).expect("fixture");
        }
        let mut workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
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
        workspace.ignore.custom_patterns = vec!["c.txt".to_string()];

        let error = walk_page(&root, &workspace, Some(&cursor), options)
            .expect_err("custom ignore plan change must invalidate the cursor");
        assert!(error.to_string().contains("ignore plan"));
    }

    #[test]
    fn unfenced_git_exclude_source_does_not_own_traversal_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        std::fs::create_dir_all(root.join(".git/info")).expect("git metadata fixture");
        std::fs::write(root.join(".git/info/exclude"), "hidden.txt\n")
            .expect("git exclude fixture");
        std::fs::write(root.join("hidden.txt"), "hidden").expect("hidden fixture");
        std::fs::write(root.join("visible.txt"), "visible").expect("visible fixture");
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
                include_directories: false,
                result_limit: 8,
                visit_limit: 32,
            },
        )
        .expect("bounded traversal");

        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["hidden.txt", "visible.txt"]
        );
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

    #[test]
    fn opaque_continuation_registry_is_one_shot_and_bounded() {
        let registry = TraversalRegistry::default();
        let first = registry
            .register_one_shot_continuation("fixture", vec![0])
            .expect("first token");
        let mut last = first.clone();
        for value in 1..=MAX_ACTIVE_ONE_SHOT_CONTINUATIONS {
            last = registry
                .register_one_shot_continuation("fixture", vec![(value % 255) as u8])
                .expect("bounded token");
        }

        assert!(
            registry
                .consume_one_shot_continuation("fixture", &first)
                .is_err(),
            "the oldest token is evicted at the fixed registry bound"
        );
        assert_eq!(
            registry
                .consume_one_shot_continuation("fixture", &last)
                .expect("latest token"),
            vec![(MAX_ACTIVE_ONE_SHOT_CONTINUATIONS % 255) as u8]
        );
        assert!(
            registry
                .consume_one_shot_continuation("fixture", &last)
                .is_err(),
            "a consumed token cannot be replayed"
        );
    }

    #[test]
    fn custom_ignored_subtree_is_pruned_before_visit_budget_and_cursor_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        std::fs::create_dir_all(root.join("00-hidden/deep/more")).expect("hidden tree");
        for index in 0..8 {
            std::fs::write(
                root.join(format!("00-hidden/deep/more/{index}.txt")),
                "hidden",
            )
            .expect("hidden fixture");
        }
        std::fs::write(root.join("visible-a.txt"), "a").expect("visible fixture");
        std::fs::write(root.join("visible-b.txt"), "b").expect("visible fixture");
        let mut config = ResolvedConfig::default();
        config.workspace.extra_ignore_globs = vec!["00-hidden/**".to_string()];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        let page = walk_page(
            &root,
            &workspace,
            None,
            TraversalOptions {
                include_hidden: false,
                max_depth: None,
                include_files: true,
                include_directories: false,
                result_limit: 10,
                visit_limit: 3,
            },
        )
        .expect("pruned traversal");

        assert_eq!(page.visited_entries, 3, "root plus two visible files");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["visible-a.txt", "visible-b.txt"]
        );
        assert!(page.continuation.is_none());
        assert!(!page.truncated);
    }

    #[test]
    fn protected_subtree_does_not_own_cursor_stamps_or_resume_budget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let protected = root.join("00-protected");
        std::fs::create_dir_all(protected.join("deep")).expect("protected tree");
        std::fs::write(protected.join("deep/secret.txt"), "secret").expect("protected fixture");
        for name in ["visible-a.txt", "visible-b.txt", "visible-c.txt"] {
            std::fs::write(root.join(name), name).expect("visible fixture");
        }
        let mut config = ResolvedConfig::default();
        config.workspace.protected_paths = vec![protected.clone()];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 4,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(protected.join("deep/changed.txt"), "changed")
            .expect("mutate protected subtree");
        let second = walk_page(&root, &workspace, Some(&cursor), options)
            .expect("protected subtree mutation must not stale the visible cursor");

        assert_eq!(first.entries[0].relative_path, "visible-a.txt");
        assert_eq!(second.entries[0].relative_path, "visible-b.txt");
        assert!(first.visited_entries <= 3);
        assert!(second.visited_entries <= 3);
    }

    #[cfg(windows)]
    #[test]
    fn continuation_root_identity_and_relative_projection_ignore_windows_case() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        for name in ["a.txt", "b.txt"] {
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
            visit_limit: 8,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");
        let root_case_variant = Utf8PathBuf::from(root.as_str().to_ascii_uppercase());

        let second = walk_page(&root_case_variant, &workspace, Some(&cursor), options)
            .expect("case variant is the same Windows root");

        assert_eq!(second.entries[0].relative_path, "b.txt");
        assert!(!second.entries[0].relative_path.is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn continuation_rejects_a_case_distinct_root_in_a_case_sensitive_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent =
            Utf8PathBuf::from_path_buf(temp.path().join("case-parent")).expect("utf8 case parent");
        std::fs::create_dir(&parent).expect("case parent");
        enable_case_sensitive_directory(&parent);
        let root = parent.join("Root");
        let sibling = parent.join("root");
        std::fs::create_dir(&root).expect("cursor root");
        std::fs::create_dir(&sibling).expect("case-distinct sibling root");
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), name).expect("cursor fixture");
            std::fs::write(sibling.join(name), name).expect("sibling fixture");
        }
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&parent, &ResolvedConfig::default())
                .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 8,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");

        let error = walk_page(&sibling, &workspace, Some(&cursor), options)
            .expect_err("case-distinct sibling must not inherit the cursor owner");

        assert!(error.to_string().contains("different root"));
    }

    #[cfg(windows)]
    #[test]
    fn case_distinct_files_resume_in_exact_walk_order_without_duplicates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("case-root")).expect("utf8 root");
        std::fs::create_dir(&root).expect("case-sensitive root");
        enable_case_sensitive_directory(&root);
        let upper = root.join("A.txt");
        let lower = root.join("a.txt");
        std::fs::write(&upper, "upper").expect("upper fixture");
        std::fs::write(&lower, "lower").expect("lower fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 8,
        };

        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.clone().expect("continuation");
        let second = walk_page(&root, &workspace, Some(&cursor), options).expect("second page");
        let actual = first
            .entries
            .iter()
            .chain(second.entries.iter())
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let mut expected = vec![upper, lower];
        expected
            .sort_by(|left, right| compare_traversal_path(left.as_std_path(), right.as_std_path()));

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 2);
        assert_ne!(actual[0], actual[1]);
        assert!(second.continuation.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn continuation_accepts_a_unicode_case_alias_of_the_same_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let container =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 container");
        let root = container.join("ÜnicodeRoot");
        std::fs::create_dir(&root).expect("Unicode root");
        for name in ["a.txt", "b.txt"] {
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
            visit_limit: 8,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let cursor = first.continuation.expect("continuation");
        let alias = container.join("ünicoderoot");

        let second = walk_page(&alias, &workspace, Some(&cursor), options)
            .expect("Unicode case alias is the same Windows root object");

        assert_eq!(second.entries[0].relative_path, "b.txt");
        assert!(!second.entries[0].relative_path.is_absolute());
    }

    #[cfg(windows)]
    #[test]
    fn traversal_hides_case_variant_of_configured_protected_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        std::fs::create_dir_all(root.join(".MOYAI/RuLeS-Team"))
            .expect("protected fixture directory");
        std::fs::write(root.join(".MOYAI/RuLeS-Team/policy.md"), "protected")
            .expect("protected fixture");
        std::fs::write(root.join("visible.txt"), "visible").expect("visible fixture");
        let mut workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
                .expect("workspace");
        workspace.protected_paths.push(root.join(".moyai"));

        let page = walk_page(
            &root,
            &workspace,
            None,
            TraversalOptions {
                include_hidden: true,
                max_depth: None,
                include_files: true,
                include_directories: true,
                result_limit: 16,
                visit_limit: 32,
            },
        )
        .expect("bounded page");

        let relative_paths = page
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(relative_paths, vec!["visible.txt"]);
    }
}
