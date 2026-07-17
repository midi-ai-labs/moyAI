use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::StorageError;
use crate::storage::{
    InternalFileMaintenanceLease, SqliteChangeRepository, SqliteProjectRepository,
    SqliteSessionRepository, StoragePaths,
};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const INTERNAL_FILE_CLEANUP_BATCH_SIZE: usize = 64;
const INTERNAL_FILE_ROOT_BATCH_SIZE: usize = INTERNAL_FILE_CLEANUP_BATCH_SIZE / 2;
const INTERNAL_FILE_QUARANTINE_DIR: &str = ".internal-file-quarantine";
const INTERNAL_FILE_QUARANTINE_TICK_BUDGET: usize = 64;

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    paths: StoragePaths,
    internal_file_cleanup_cursor: Arc<Mutex<InternalFileCleanupCursor>>,
    #[cfg(test)]
    cleanup_test_hook: Arc<Mutex<Option<CleanupTestHook>>>,
}

#[derive(Default)]
struct InternalFileCleanupCursor {
    harness: DirectoryReadCursor,
    truncation: DirectoryReadCursor,
    quarantine: QuarantineDrainCursor,
}

#[derive(Default)]
struct DirectoryReadCursor {
    entries: Option<fs::ReadDir>,
    root: Option<StableDirectoryHandle>,
}

struct DirectoryEntryBatch {
    entries: Vec<DirectoryCandidate>,
    examined: usize,
    cycle_complete: bool,
    root: Option<StableDirectoryHandle>,
}

#[derive(Default)]
struct QuarantineDrainCursor {
    root_entries: Option<fs::ReadDir>,
    root: Option<StableDirectoryHandle>,
    stack: Vec<QuarantineDirectoryFrame>,
}

struct QuarantineDirectoryFrame {
    directory: StableDirectoryHandle,
    parent: StableDirectoryHandle,
    name: OsString,
    removable: bool,
    entries: fs::ReadDir,
}

#[derive(Clone)]
struct StableDirectoryHandle {
    path: PathBuf,
    identity: InternalDirectoryIdentity,
    file: Arc<fs::File>,
}

#[derive(Debug)]
struct StableEntryHandle {
    file: fs::File,
    identity: InternalEntryIdentity,
}

struct DirectoryCandidate {
    name: OsString,
    identity: InternalEntryIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
enum InternalEntryKind {
    File,
    Directory,
    LinkLike,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalEntryIdentity {
    kind: InternalEntryKind,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalDirectoryIdentity {
    canonical_path: PathBuf,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableMutationOutcome {
    Mutated,
    Missing,
    Changed,
}

#[derive(Debug)]
enum StableEntryOpenOutcome {
    Opened(StableEntryHandle),
    Missing,
    Changed,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupTestHookPoint {
    BeforeLiveEntryOpen,
    AfterLiveEntryOpen,
    AfterDrainEntryOpen,
}

#[cfg(test)]
struct CleanupTestHook {
    point: CleanupTestHookPoint,
    reached: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageMaintenanceReport {
    pub orphan_harness_dirs_quarantined: usize,
    pub orphan_truncation_files_quarantined: usize,
    pub live_quarantine_failures: usize,
    pub harness_entries_examined: usize,
    pub truncation_entries_examined: usize,
    pub quarantine_entries_examined: usize,
    pub quarantine_mutations_attempted: usize,
    pub quarantine_entries_removed: usize,
    pub quarantine_failures: usize,
    pub harness_cycle_complete: bool,
    pub truncation_cycle_complete: bool,
    pub quarantine_cycle_complete: bool,
}

impl SqliteStore {
    pub fn open(paths: &StoragePaths) -> Result<Self, StorageError> {
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.truncation_dir)?;
        let connection = Connection::open(&paths.database_path)?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            paths: paths.clone(),
            internal_file_cleanup_cursor: Arc::new(
                Mutex::new(InternalFileCleanupCursor::default()),
            ),
            #[cfg(test)]
            cleanup_test_hook: Arc::new(Mutex::new(None)),
        })
    }

    pub fn migrate(&self) -> Result<(), StorageError> {
        crate::storage::migration::run(&self.connection.lock().expect("sqlite mutex poisoned"))
    }

    pub fn session_repo(&self) -> SqliteSessionRepository {
        SqliteSessionRepository::new(self.connection.clone())
    }

    pub fn project_repo(&self) -> SqliteProjectRepository {
        SqliteProjectRepository::new(self.connection.clone())
    }

    pub fn change_repo(&self) -> SqliteChangeRepository {
        SqliteChangeRepository::new(self.connection.clone())
    }

    pub fn harness_event_store(&self) -> crate::harness::SqliteHarnessEventStore {
        crate::harness::SqliteHarnessEventStore::new(self.connection.clone())
    }

    pub fn harness_run_store(&self) -> crate::harness::SqliteHarnessRunStore {
        crate::harness::SqliteHarnessRunStore::new(self.connection.clone())
    }

    pub fn harness_artifact_store(&self) -> crate::harness::SqliteArtifactStore {
        crate::harness::SqliteArtifactStore::new(self.connection.clone())
    }

    pub fn harness_contract_store(&self) -> crate::harness::SqliteContractStore {
        crate::harness::SqliteContractStore::new(self.connection.clone())
    }

    pub fn harness_gate_result_store(&self) -> crate::harness::SqliteGateResultStore {
        crate::harness::SqliteGateResultStore::new(self.connection.clone())
    }

    pub fn harness_replay_report_store(&self) -> crate::harness::SqliteReplayReportStore {
        crate::harness::SqliteReplayReportStore::new(self.connection.clone())
    }

    pub fn protocol_event_store(&self) -> crate::protocol::SqliteProtocolEventStore {
        crate::protocol::SqliteProtocolEventStore::new(self.connection.clone())
    }

    pub fn cleanup_orphan_internal_files(&self) -> Result<StorageMaintenanceReport, StorageError> {
        let harness_root = self.paths.data_dir.join("harness");
        let quarantine_root = self.paths.data_dir.join(INTERNAL_FILE_QUARANTINE_DIR);
        let quarantine_harness_root = quarantine_root.join("harness");
        let quarantine_truncation_root = quarantine_root.join("truncation");
        let data_root = open_internal_data_root(self.paths.data_dir.as_std_path())?;
        let quarantine_root_handle =
            ensure_internal_directory(quarantine_root.as_std_path(), &data_root)?;
        let quarantine_harness_handle =
            ensure_internal_directory(quarantine_harness_root.as_std_path(), &data_root)?;
        let quarantine_truncation_handle =
            ensure_internal_directory(quarantine_truncation_root.as_std_path(), &data_root)?;

        // The shared process-local ReadDir cursors are the only traversal owner. Candidate
        // acquisition stays outside the producer fence and performs a fixed number of `next`
        // calls. A process restart merely restarts an idempotent directory cycle.
        let (harness_batch, truncation_batch) = {
            let mut cursor = self
                .internal_file_cleanup_cursor
                .lock()
                .expect("internal file cleanup cursor mutex poisoned");
            let harness_batch = next_directory_entry_batch(
                &mut cursor.harness,
                harness_root.as_std_path(),
                &data_root,
                INTERNAL_FILE_ROOT_BATCH_SIZE,
            )?;
            let truncation_batch = next_directory_entry_batch(
                &mut cursor.truncation,
                self.paths.truncation_dir.as_std_path(),
                &data_root,
                INTERNAL_FILE_ROOT_BATCH_SIZE,
            )?;
            (harness_batch, truncation_batch)
        };

        let mut report = StorageMaintenanceReport {
            harness_entries_examined: harness_batch.examined,
            truncation_entries_examined: truncation_batch.examined,
            harness_cycle_complete: harness_batch.cycle_complete,
            truncation_cycle_complete: truncation_batch.cycle_complete,
            ..StorageMaintenanceReport::default()
        };

        // Producers hold the shared counterpart from before file creation until the exact
        // database owner commits. Under the exclusive counterpart cleanup only re-stats the
        // bounded candidates, performs exact owner lookups, and atomically renames orphans within
        // the data volume. Recursive work is deferred until after this scope.
        {
            let _maintenance_lease = InternalFileMaintenanceLease::acquire(&self.paths)?;
            require_same_internal_directory(
                quarantine_root.as_std_path(),
                &data_root,
                &quarantine_root_handle.identity,
            )?;
            require_same_internal_directory(
                quarantine_harness_root.as_std_path(),
                &data_root,
                &quarantine_harness_handle.identity,
            )?;
            require_same_internal_directory(
                quarantine_truncation_root.as_std_path(),
                &data_root,
                &quarantine_truncation_handle.identity,
            )?;
            let mut rename_budget = INTERNAL_FILE_CLEANUP_BATCH_SIZE;
            if batch_root_is_current(
                harness_root.as_std_path(),
                &data_root,
                harness_batch.root.as_ref(),
            )? {
                if let Some(source_root) = harness_batch.root.as_ref() {
                    self.quarantine_harness_entries(
                        harness_batch.entries,
                        source_root,
                        &quarantine_harness_handle,
                        &mut rename_budget,
                        &mut report,
                    )?;
                }
            } else if !harness_batch.entries.is_empty() {
                report.live_quarantine_failures += 1;
            }
            if batch_root_is_current(
                self.paths.truncation_dir.as_std_path(),
                &data_root,
                truncation_batch.root.as_ref(),
            )? {
                if let Some(source_root) = truncation_batch.root.as_ref() {
                    self.quarantine_truncation_entries(
                        truncation_batch.entries,
                        source_root,
                        &quarantine_truncation_handle,
                        &mut rename_budget,
                        &mut report,
                    )?;
                }
            } else if !truncation_batch.entries.is_empty() {
                report.live_quarantine_failures += 1;
            }
        }

        {
            require_same_internal_directory(
                quarantine_root.as_std_path(),
                &data_root,
                &quarantine_root_handle.identity,
            )?;
            require_same_internal_directory(
                quarantine_harness_root.as_std_path(),
                &data_root,
                &quarantine_harness_handle.identity,
            )?;
            require_same_internal_directory(
                quarantine_truncation_root.as_std_path(),
                &data_root,
                &quarantine_truncation_handle.identity,
            )?;
            // The live-rename destination pins intentionally deny share-delete. Release the two
            // namespace pins before the drain attempts to remove an emptied namespace by its own
            // exact handle. The quarantine root itself remains pinned as the drain parent.
            drop(quarantine_harness_handle);
            drop(quarantine_truncation_handle);
            let mut cursor = self
                .internal_file_cleanup_cursor
                .lock()
                .expect("internal file cleanup cursor mutex poisoned");
            drain_quarantine_tick(
                self,
                &mut cursor.quarantine,
                &quarantine_root_handle,
                INTERNAL_FILE_QUARANTINE_TICK_BUDGET,
                &mut report,
            )?;
        }

        Ok(report)
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }

    fn quarantine_harness_entries(
        &self,
        entries: Vec<DirectoryCandidate>,
        harness_root: &StableDirectoryHandle,
        quarantine_root: &StableDirectoryHandle,
        rename_budget: &mut usize,
        report: &mut StorageMaintenanceReport,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT EXISTS(
                 SELECT 1
                 FROM harness_runs
                 WHERE id = ?1 AND artifact_root = ?2
             )",
        )?;
        for entry in entries {
            if *rename_budget == 0 {
                break;
            }
            if entry.identity.kind != InternalEntryKind::Directory {
                continue;
            }
            let path = harness_root.path.join(&entry.name);
            let (Some(run_id), Some(artifact_root)) = (entry.name.to_str(), path.to_str()) else {
                // An unrepresentable path cannot be compared with the UTF-8 durable owner. Keep it
                // in place rather than interpreting an encoding failure as absence of ownership.
                report.live_quarantine_failures += 1;
                continue;
            };
            let referenced =
                statement.query_row([run_id, artifact_root], |row| row.get::<_, bool>(0))?;
            if !referenced {
                #[cfg(test)]
                self.pause_cleanup_test_hook(CleanupTestHookPoint::BeforeLiveEntryOpen);
                let opened = match open_stable_entry(harness_root, &entry) {
                    Ok(StableEntryOpenOutcome::Opened(opened)) => opened,
                    Ok(StableEntryOpenOutcome::Missing) => continue,
                    Ok(StableEntryOpenOutcome::Changed) => {
                        report.live_quarantine_failures += 1;
                        continue;
                    }
                    Err(_) => {
                        report.live_quarantine_failures += 1;
                        continue;
                    }
                };
                #[cfg(test)]
                self.pause_cleanup_test_hook(CleanupTestHookPoint::AfterLiveEntryOpen);
                *rename_budget -= 1;
                let destination = OsString::from(ulid::Ulid::new().to_string());
                match rename_stable_entry(
                    harness_root,
                    &entry.name,
                    &opened,
                    quarantine_root,
                    &destination,
                ) {
                    Ok(StableMutationOutcome::Mutated) => {
                        report.orphan_harness_dirs_quarantined += 1
                    }
                    Ok(StableMutationOutcome::Missing) => {}
                    Ok(StableMutationOutcome::Changed) => report.live_quarantine_failures += 1,
                    Err(_) => report.live_quarantine_failures += 1,
                }
            }
        }
        Ok(())
    }

    fn quarantine_truncation_entries(
        &self,
        entries: Vec<DirectoryCandidate>,
        truncation_root: &StableDirectoryHandle,
        quarantine_root: &StableDirectoryHandle,
        rename_budget: &mut usize,
        report: &mut StorageMaintenanceReport,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT EXISTS(
                 SELECT 1
                 FROM tool_calls INDEXED BY idx_tool_calls_truncated_output_path
                 WHERE truncated_output_path IS NOT NULL
                   AND truncated_output_path = ?1
             )",
        )?;
        for entry in entries {
            if *rename_budget == 0 {
                break;
            }
            if entry.identity.kind != InternalEntryKind::File {
                continue;
            }
            let path = truncation_root.path.join(&entry.name);
            let Some(path_text) = path.to_str() else {
                report.live_quarantine_failures += 1;
                continue;
            };
            let referenced = statement.query_row([path_text], |row| row.get::<_, bool>(0))?;
            if !referenced {
                #[cfg(test)]
                self.pause_cleanup_test_hook(CleanupTestHookPoint::BeforeLiveEntryOpen);
                let opened = match open_stable_entry(truncation_root, &entry) {
                    Ok(StableEntryOpenOutcome::Opened(opened)) => opened,
                    Ok(StableEntryOpenOutcome::Missing) => continue,
                    Ok(StableEntryOpenOutcome::Changed) => {
                        report.live_quarantine_failures += 1;
                        continue;
                    }
                    Err(_) => {
                        report.live_quarantine_failures += 1;
                        continue;
                    }
                };
                #[cfg(test)]
                self.pause_cleanup_test_hook(CleanupTestHookPoint::AfterLiveEntryOpen);
                *rename_budget -= 1;
                let destination = OsString::from(ulid::Ulid::new().to_string());
                match rename_stable_entry(
                    truncation_root,
                    &entry.name,
                    &opened,
                    quarantine_root,
                    &destination,
                ) {
                    Ok(StableMutationOutcome::Mutated) => {
                        report.orphan_truncation_files_quarantined += 1
                    }
                    Ok(StableMutationOutcome::Missing) => {}
                    Ok(StableMutationOutcome::Changed) => report.live_quarantine_failures += 1,
                    Err(_) => report.live_quarantine_failures += 1,
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn install_cleanup_test_hook(
        &self,
        point: CleanupTestHookPoint,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        *self
            .cleanup_test_hook
            .lock()
            .expect("cleanup test hook mutex poisoned") = Some(CleanupTestHook {
            point,
            reached: reached_tx,
            resume: resume_rx,
        });
        (reached_rx, resume_tx)
    }

    #[cfg(test)]
    fn pause_cleanup_test_hook(&self, point: CleanupTestHookPoint) {
        let hook = {
            let mut slot = self
                .cleanup_test_hook
                .lock()
                .expect("cleanup test hook mutex poisoned");
            if slot.as_ref().is_some_and(|hook| hook.point == point) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook.reached.send(()).expect("cleanup hook reached");
            hook.resume.recv().expect("cleanup hook resumed");
        }
    }
}

fn open_internal_data_root(data_root: &Path) -> Result<StableDirectoryHandle, StorageError> {
    let metadata = fs::symlink_metadata(data_root)?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(StorageError::Message(format!(
            "internal file data root must be a non-link directory: {}",
            data_root.display()
        )));
    }
    #[cfg(unix)]
    let observed_identity = entry_identity_from_metadata(&metadata);
    let file = open_absolute_directory(data_root)?;
    let opened_identity = entry_identity_from_file(&file)?;
    #[cfg(unix)]
    if opened_identity != observed_identity {
        return Err(StorageError::Message(format!(
            "internal file data root changed while it was opened: {}",
            data_root.display()
        )));
    }
    let canonical_path = fs::canonicalize(data_root)?;
    let identity = directory_identity_from_entry(canonical_path, &opened_identity, data_root)?;
    let stable = StableDirectoryHandle {
        path: data_root.to_path_buf(),
        identity,
        file: Arc::new(file),
    };
    let reopened = open_absolute_directory(data_root)?;
    let reopened_identity = directory_identity_from_entry(
        stable.identity.canonical_path.clone(),
        &entry_identity_from_file(&reopened)?,
        data_root,
    )?;
    if reopened_identity != stable.identity {
        return Err(StorageError::Message(format!(
            "internal file data root changed while it was pinned: {}",
            data_root.display()
        )));
    }
    Ok(stable)
}

fn ensure_internal_directory(
    path: &Path,
    data_root: &StableDirectoryHandle,
) -> Result<StableDirectoryHandle, StorageError> {
    if let Some(directory) = open_internal_directory(path, data_root)? {
        return Ok(directory);
    }
    let parent_path = path.parent().ok_or_else(|| {
        StorageError::Message(format!(
            "internal file directory has no parent: {}",
            path.display()
        ))
    })?;
    let parent = open_internal_directory(parent_path, data_root)?.ok_or_else(|| {
        StorageError::Message(format!(
            "internal file directory parent disappeared: {}",
            parent_path.display()
        ))
    })?;
    let name = path.file_name().ok_or_else(|| {
        StorageError::Message(format!(
            "internal file directory has no name: {}",
            path.display()
        ))
    })?;
    create_relative_directory(&parent, name)?;
    open_internal_directory(path, data_root)?.ok_or_else(|| {
        StorageError::Message(format!(
            "internal file directory disappeared during creation: {}",
            path.display()
        ))
    })
}

fn require_same_internal_directory(
    path: &Path,
    data_root: &StableDirectoryHandle,
    expected: &InternalDirectoryIdentity,
) -> Result<(), StorageError> {
    let current = open_internal_directory(path, data_root)?;
    if current.as_ref().map(|directory| &directory.identity) != Some(expected) {
        return Err(StorageError::Message(format!(
            "internal file directory identity changed: {}",
            path.display()
        )));
    }
    Ok(())
}

fn batch_root_is_current(
    path: &Path,
    data_root: &StableDirectoryHandle,
    expected: Option<&StableDirectoryHandle>,
) -> Result<bool, StorageError> {
    let current = open_internal_directory(path, data_root)?;
    Ok(current.as_ref().map(|directory| &directory.identity)
        == expected.map(|directory| &directory.identity))
}

fn open_internal_directory(
    path: &Path,
    data_root: &StableDirectoryHandle,
) -> Result<Option<StableDirectoryHandle>, StorageError> {
    let relative = path.strip_prefix(&data_root.path).map_err(|_| {
        StorageError::Message(format!(
            "internal file directory is outside the configured data root: {}",
            path.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Ok(Some(data_root.clone()));
    }
    let mut current = data_root.clone();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(StorageError::Message(format!(
                "internal file directory contains a non-normal component: {}",
                path.display()
            )));
        };
        let next_path = current.path.join(name);
        let file = match open_relative_directory_file(&current, name) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let canonical_path = current.identity.canonical_path.join(name);
        let identity = directory_identity_from_entry(
            canonical_path,
            &entry_identity_from_file(&file)?,
            &next_path,
        )?;
        current = StableDirectoryHandle {
            path: next_path,
            identity,
            file: Arc::new(file),
        };
    }
    Ok(Some(current))
}

fn directory_identity_from_entry(
    canonical_path: PathBuf,
    entry: &InternalEntryIdentity,
    display_path: &Path,
) -> Result<InternalDirectoryIdentity, StorageError> {
    if entry.kind != InternalEntryKind::Directory {
        return Err(StorageError::Message(format!(
            "internal file root must be a non-link directory: {}",
            display_path.display()
        )));
    }
    Ok(InternalDirectoryIdentity {
        canonical_path,
        #[cfg(windows)]
        volume_serial_number: entry.volume_serial_number,
        #[cfg(windows)]
        file_index: entry.file_index,
        #[cfg(unix)]
        device: entry.device,
        #[cfg(unix)]
        inode: entry.inode,
    })
}

fn directory_identity_matches_entry(
    directory: &InternalDirectoryIdentity,
    entry: &InternalEntryIdentity,
) -> bool {
    entry.kind == InternalEntryKind::Directory && {
        #[cfg(windows)]
        {
            directory.volume_serial_number == entry.volume_serial_number
                && directory.file_index == entry.file_index
        }
        #[cfg(unix)]
        {
            directory.device == entry.device && directory.inode == entry.inode
        }
        #[cfg(not(any(unix, windows)))]
        {
            true
        }
    }
}

fn stable_directory_path_is_current(directory: &StableDirectoryHandle) -> std::io::Result<bool> {
    let current = match open_absolute_directory(&directory.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(directory_identity_matches_entry(
        &directory.identity,
        &entry_identity_from_file(&current)?,
    ))
}

fn validate_relative_name(name: &OsStr) -> std::io::Result<()> {
    let mut components = Path::new(name).components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "internal file entry name must be one normal path component",
        ))
    }
}

fn create_relative_directory(
    parent: &StableDirectoryHandle,
    name: &OsStr,
) -> Result<(), StorageError> {
    validate_relative_name(name)?;
    #[cfg(windows)]
    let result = fs::create_dir(parent.path.join(name));
    #[cfg(unix)]
    let result = {
        use std::os::unix::io::AsRawFd as _;

        let name = unix_relative_name(name)?;
        // SAFETY: `parent.file` owns a live directory descriptor and `name` is a NUL-terminated
        // single component. mkdirat cannot traverse a replacement of the configured parent path.
        let status = unsafe { libc::mkdirat(parent.file.as_raw_fd(), name.as_ptr(), 0o700) };
        if status == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    #[cfg(not(any(unix, windows)))]
    let result = fs::create_dir(parent.path.join(name));
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_absolute_directory(path: &Path) -> std::io::Result<fs::File> {
    #[cfg(windows)]
    {
        open_windows_directory(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::io::FromRawFd as _;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "internal directory path contains NUL",
            )
        })?;
        // SAFETY: `path` is a valid C string. O_NOFOLLOW rejects a link at the final component;
        // this descriptor becomes the stable owner for every subsequent relative operation.
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: `descriptor` was returned by open and ownership transfers to File exactly
            // once.
            Ok(unsafe { fs::File::from_raw_fd(descriptor) })
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::File::open(path)
    }
}

fn open_relative_directory_file(
    parent: &StableDirectoryHandle,
    name: &OsStr,
) -> std::io::Result<fs::File> {
    validate_relative_name(name)?;
    #[cfg(windows)]
    {
        open_windows_directory(&parent.path.join(name))
    }
    #[cfg(unix)]
    {
        unix_open_at(
            parent,
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::File::open(parent.path.join(name))
    }
}

fn entry_identity_from_file(file: &fs::File) -> std::io::Result<InternalEntryIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            GetFileInformationByHandle,
        };

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `file` keeps the handle live and `information` is the exact writable output
        // structure required by GetFileInformationByHandle.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let kind = if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            InternalEntryKind::LinkLike
        } else if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            InternalEntryKind::Directory
        } else {
            InternalEntryKind::File
        };
        Ok(InternalEntryIdentity {
            kind,
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = file.metadata()?;
        let kind = if metadata.is_dir() {
            InternalEntryKind::Directory
        } else if metadata.is_file() {
            InternalEntryKind::File
        } else if metadata.file_type().is_symlink() {
            InternalEntryKind::LinkLike
        } else {
            InternalEntryKind::Other
        };
        Ok(InternalEntryIdentity {
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = file.metadata()?;
        let kind = if metadata.is_dir() {
            InternalEntryKind::Directory
        } else if metadata.is_file() {
            InternalEntryKind::File
        } else {
            InternalEntryKind::Other
        };
        Ok(InternalEntryIdentity { kind })
    }
}

#[cfg(unix)]
fn entry_identity_from_metadata(metadata: &fs::Metadata) -> InternalEntryIdentity {
    use std::os::unix::fs::MetadataExt as _;

    let kind = if metadata.file_type().is_symlink() {
        InternalEntryKind::LinkLike
    } else if metadata.is_dir() {
        InternalEntryKind::Directory
    } else if metadata.is_file() {
        InternalEntryKind::File
    } else {
        InternalEntryKind::Other
    };
    InternalEntryIdentity {
        kind,
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn relative_entry_identity(
    parent: &StableDirectoryHandle,
    name: &OsStr,
) -> std::io::Result<Option<InternalEntryIdentity>> {
    validate_relative_name(name)?;
    #[cfg(windows)]
    {
        match open_windows_observation_entry(&parent.path.join(name)) {
            Ok(file) => entry_identity_from_file(&file).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    #[cfg(unix)]
    {
        unix_identity_at(parent, name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        match fs::symlink_metadata(parent.path.join(name)) {
            Ok(metadata) => {
                let kind = if metadata.file_type().is_symlink() {
                    InternalEntryKind::LinkLike
                } else if metadata.is_dir() {
                    InternalEntryKind::Directory
                } else if metadata.is_file() {
                    InternalEntryKind::File
                } else {
                    InternalEntryKind::Other
                };
                Ok(Some(InternalEntryIdentity { kind }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn open_stable_entry(
    parent: &StableDirectoryHandle,
    candidate: &DirectoryCandidate,
) -> std::io::Result<StableEntryOpenOutcome> {
    validate_relative_name(&candidate.name)?;
    if !matches!(
        candidate.identity.kind,
        InternalEntryKind::File | InternalEntryKind::Directory
    ) {
        return Ok(StableEntryOpenOutcome::Changed);
    }
    #[cfg(windows)]
    let opened = open_windows_mutation_entry(&parent.path.join(&candidate.name));
    #[cfg(unix)]
    let opened = unix_open_at(
        parent,
        &candidate.name,
        libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if candidate.identity.kind == InternalEntryKind::Directory {
                libc::O_DIRECTORY
            } else {
                0
            },
    );
    #[cfg(not(any(unix, windows)))]
    let opened = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable internal entry handles are unsupported on this platform",
    ));
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StableEntryOpenOutcome::Missing);
        }
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Ok(StableEntryOpenOutcome::Changed);
        }
        Err(error) => return Err(error),
    };
    let identity = entry_identity_from_file(&file)?;
    if identity != candidate.identity {
        return Ok(StableEntryOpenOutcome::Changed);
    }
    Ok(StableEntryOpenOutcome::Opened(StableEntryHandle {
        file,
        identity,
    }))
}

fn open_stable_directory_candidate(
    parent: &StableDirectoryHandle,
    candidate: &DirectoryCandidate,
) -> std::io::Result<Option<StableDirectoryHandle>> {
    if candidate.identity.kind != InternalEntryKind::Directory {
        return Ok(None);
    }
    let file = match open_relative_directory_file(parent, &candidate.name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let identity = entry_identity_from_file(&file)?;
    if identity != candidate.identity {
        return Ok(None);
    }
    stable_directory_from_entry(
        parent,
        &candidate.name,
        StableEntryHandle { file, identity },
    )
    .map(Some)
    .map_err(|error| std::io::Error::other(error.to_string()))
}

fn stable_directory_from_entry(
    parent: &StableDirectoryHandle,
    name: &OsStr,
    entry: StableEntryHandle,
) -> Result<StableDirectoryHandle, StorageError> {
    let path = parent.path.join(name);
    let identity = directory_identity_from_entry(
        parent.identity.canonical_path.join(name),
        &entry.identity,
        &path,
    )?;
    Ok(StableDirectoryHandle {
        path,
        identity,
        file: Arc::new(entry.file),
    })
}

fn rename_stable_entry(
    source_parent: &StableDirectoryHandle,
    source_name: &OsStr,
    opened: &StableEntryHandle,
    destination_parent: &StableDirectoryHandle,
    destination_name: &OsStr,
) -> std::io::Result<StableMutationOutcome> {
    validate_relative_name(source_name)?;
    validate_relative_name(destination_name)?;
    match relative_entry_identity(source_parent, source_name)? {
        None => return Ok(StableMutationOutcome::Missing),
        Some(current) if current != opened.identity => {
            return Ok(StableMutationOutcome::Changed);
        }
        Some(_) => {}
    }
    #[cfg(windows)]
    let result = windows_rename_by_handle(&opened.file, &destination_parent.file, destination_name);
    #[cfg(unix)]
    let result = unix_rename_at(
        source_parent,
        source_name,
        destination_parent,
        destination_name,
    );
    #[cfg(not(any(unix, windows)))]
    let result = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable internal entry rename is unsupported on this platform",
    ));
    match result {
        Ok(()) => Ok(StableMutationOutcome::Mutated),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(StableMutationOutcome::Missing)
        }
        Err(error) => Err(error),
    }
}

fn delete_stable_entry(
    parent: &StableDirectoryHandle,
    name: &OsStr,
    opened: &StableEntryHandle,
) -> std::io::Result<StableMutationOutcome> {
    validate_relative_name(name)?;
    match relative_entry_identity(parent, name)? {
        None => return Ok(StableMutationOutcome::Missing),
        Some(current) if current != opened.identity => {
            return Ok(StableMutationOutcome::Changed);
        }
        Some(_) => {}
    }
    #[cfg(windows)]
    let result = windows_delete_by_handle(&opened.file);
    #[cfg(unix)]
    let result = unix_unlink_at(
        parent,
        name,
        opened.identity.kind == InternalEntryKind::Directory,
    );
    #[cfg(not(any(unix, windows)))]
    let result = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable internal entry delete is unsupported on this platform",
    ));
    match result {
        Ok(()) => Ok(StableMutationOutcome::Mutated),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(StableMutationOutcome::Missing)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn open_windows_directory(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_APPEND_DATA, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_DATA,
        SYNCHRONIZE,
    };

    fs::OpenOptions::new()
        .access_mode(
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE | FILE_WRITE_DATA | FILE_APPEND_DATA | SYNCHRONIZE,
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn open_windows_observation_entry(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };

    fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn open_windows_mutation_entry(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };

    fs::OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(windows)]
fn windows_rename_by_handle(
    source: &fs::File,
    destination_parent: &fs::File,
    destination_name: &OsStr,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FILE_RENAME_INFORMATION_0, FileRenameInformation,
        NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut wide = destination_name.encode_wide().collect::<Vec<_>>();
    let name_bytes = wide
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "internal rename destination is too long",
            )
        })?;
    wide.push(0);
    let buffer_bytes = std::mem::size_of::<FILE_RENAME_INFORMATION>()
        .checked_add(usize::try_from(name_bytes).expect("u32 name length fits usize"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "internal rename buffer is too large",
            )
        })?;
    let mut storage = vec![0usize; buffer_bytes.div_ceil(std::mem::size_of::<usize>())];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `storage` is usize-aligned and large enough for the fixed header plus every UTF-16
    // code unit. Both File handles stay live for the complete native information call.
    unsafe {
        information.write(FILE_RENAME_INFORMATION {
            Anonymous: FILE_RENAME_INFORMATION_0 {
                ReplaceIfExists: false,
            },
            RootDirectory: destination_parent.as_raw_handle(),
            FileNameLength: name_bytes,
            FileName: [0],
        });
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            wide.len(),
        );
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the handles, aligned rename buffer, and IO_STATUS_BLOCK are live and sized exactly
    // as required for the synchronous native information call.
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle(),
            &mut io_status,
            information.cast(),
            u32::try_from(buffer_bytes).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "internal rename buffer exceeds the native API limit",
                )
            })?,
            FileRenameInformation,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        // SAFETY: conversion accepts any NTSTATUS returned by NtSetInformationFile.
        let code = unsafe { RtlNtStatusToDosError(status) };
        Err(std::io::Error::from_raw_os_error(code as i32))
    }
}

#[cfg(windows)]
fn windows_delete_by_handle(entry: &fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let information = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `entry` owns the exact handle being deleted and `information` is the documented
    // fixed-size input for FileDispositionInfo.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            entry.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&information).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn unix_relative_name(name: &OsStr) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;

    validate_relative_name(name)?;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "internal file entry name contains NUL",
        )
    })
}

#[cfg(unix)]
fn unix_open_at(
    parent: &StableDirectoryHandle,
    name: &OsStr,
    flags: libc::c_int,
) -> std::io::Result<fs::File> {
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _};

    let name = unix_relative_name(name)?;
    // SAFETY: `parent.file` owns a live directory fd and `name` is a NUL-terminated single
    // component. O_NOFOLLOW is included by every caller.
    let descriptor = unsafe { libc::openat(parent.file.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `descriptor` is newly returned by openat and ownership transfers once.
        Ok(unsafe { fs::File::from_raw_fd(descriptor) })
    }
}

#[cfg(unix)]
fn unix_identity_at(
    parent: &StableDirectoryHandle,
    name: &OsStr,
) -> std::io::Result<Option<InternalEntryIdentity>> {
    use std::os::unix::io::AsRawFd as _;

    let name = unix_relative_name(name)?;
    // SAFETY: the descriptor and C string are live; AT_SYMLINK_NOFOLLOW makes identity describe
    // the directory entry itself rather than a link target.
    let mut status = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            &mut status,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error);
    }
    let file_type = status.st_mode & libc::S_IFMT;
    let kind = if file_type == libc::S_IFDIR {
        InternalEntryKind::Directory
    } else if file_type == libc::S_IFREG {
        InternalEntryKind::File
    } else if file_type == libc::S_IFLNK {
        InternalEntryKind::LinkLike
    } else {
        InternalEntryKind::Other
    };
    Ok(Some(InternalEntryIdentity {
        kind,
        device: status.st_dev as u64,
        inode: status.st_ino as u64,
    }))
}

#[cfg(unix)]
fn unix_rename_at(
    source_parent: &StableDirectoryHandle,
    source_name: &OsStr,
    destination_parent: &StableDirectoryHandle,
    destination_name: &OsStr,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let source_name = unix_relative_name(source_name)?;
    let destination_name = unix_relative_name(destination_name)?;
    // SAFETY: both descriptors are stable directory fds and both names are single components.
    let result = unsafe {
        libc::renameat(
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.file.as_raw_fd(),
            destination_name.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unix_unlink_at(
    parent: &StableDirectoryHandle,
    name: &OsStr,
    directory: bool,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let name = unix_relative_name(name)?;
    // SAFETY: `parent.file` is a stable directory fd and `name` is a single component. unlinkat
    // cannot follow a replaced parent path or an intermediate link.
    let result = unsafe {
        libc::unlinkat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

fn next_directory_entry_batch(
    cursor: &mut DirectoryReadCursor,
    root: &Path,
    data_root: &StableDirectoryHandle,
    batch_size: usize,
) -> Result<DirectoryEntryBatch, StorageError> {
    let root_handle = open_internal_directory(root, data_root)?;
    if root_handle.is_none() {
        cursor.entries = None;
        cursor.root = None;
        return Ok(DirectoryEntryBatch {
            entries: Vec::new(),
            examined: 0,
            cycle_complete: true,
            root: None,
        });
    }
    let root_handle = root_handle.expect("checked stable directory handle");
    if cursor.root.as_ref().map(|root| &root.identity) != Some(&root_handle.identity) {
        cursor.entries = None;
        cursor.root = Some(root_handle.clone());
    }
    if cursor.entries.is_none() {
        match fs::read_dir(root) {
            Ok(entries) => {
                if !batch_root_is_current(root, data_root, Some(&root_handle))? {
                    return Err(StorageError::Message(format!(
                        "internal file root changed while its cursor was opened: {}",
                        root.display()
                    )));
                }
                cursor.entries = Some(entries);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cursor.root = None;
                return Ok(DirectoryEntryBatch {
                    entries: Vec::new(),
                    examined: 0,
                    cycle_complete: true,
                    root: None,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }

    let mut batch = DirectoryEntryBatch {
        entries: Vec::with_capacity(batch_size),
        examined: 0,
        cycle_complete: false,
        root: Some(root_handle.clone()),
    };
    let entries = cursor
        .entries
        .as_mut()
        .expect("directory cursor initialized");
    while batch.examined < batch_size {
        match entries.next() {
            Some(entry) => {
                batch.examined += 1;
                let entry = entry?;
                let name = entry.file_name();
                if let Some(identity) = relative_entry_identity(&root_handle, &name)? {
                    batch.entries.push(DirectoryCandidate { name, identity });
                }
            }
            None => {
                batch.cycle_complete = true;
                break;
            }
        }
    }
    if batch.cycle_complete {
        cursor.entries = None;
    }
    Ok(batch)
}

fn drain_quarantine_tick(
    store: &SqliteStore,
    cursor: &mut QuarantineDrainCursor,
    quarantine_root: &StableDirectoryHandle,
    budget: usize,
    report: &mut StorageMaintenanceReport,
) -> Result<(), StorageError> {
    if cursor.root.as_ref().map(|root| &root.identity) != Some(&quarantine_root.identity) {
        cursor.root_entries = None;
        cursor.stack.clear();
        cursor.root = Some(quarantine_root.clone());
    }
    let mut remaining = budget;
    report.quarantine_cycle_complete = false;
    while remaining >= 2 {
        if !cursor.stack.is_empty() {
            let next = cursor
                .stack
                .last_mut()
                .expect("quarantine directory frame exists")
                .entries
                .next();
            match next {
                Some(Ok(entry)) => {
                    remaining -= 1;
                    report.quarantine_entries_examined += 1;
                    let parent = cursor
                        .stack
                        .last()
                        .expect("quarantine directory frame exists")
                        .directory
                        .clone();
                    process_quarantine_entry(
                        store,
                        entry,
                        &parent,
                        true,
                        cursor,
                        &mut remaining,
                        report,
                    );
                }
                Some(Err(_)) => {
                    remaining -= 1;
                    report.quarantine_entries_examined += 1;
                    report.quarantine_failures += 1;
                }
                None => {
                    let frame = cursor.stack.pop().expect("quarantine frame exists");
                    let QuarantineDirectoryFrame {
                        directory,
                        parent,
                        name,
                        removable,
                        entries,
                    } = frame;
                    // Close the enumeration handle before marking the exact directory handle for
                    // deletion. A later bounded cycle retries a failed non-empty directory.
                    drop(entries);
                    if !removable {
                        continue;
                    }
                    remaining -= 1;
                    report.quarantine_mutations_attempted += 1;
                    let opened = match directory.file.try_clone() {
                        Ok(file) => StableEntryHandle {
                            identity: match entry_identity_from_file(&file) {
                                Ok(identity) => identity,
                                Err(_) => {
                                    report.quarantine_failures += 1;
                                    continue;
                                }
                            },
                            file,
                        },
                        Err(_) => {
                            report.quarantine_failures += 1;
                            continue;
                        }
                    };
                    match delete_stable_entry(&parent, &name, &opened) {
                        Ok(StableMutationOutcome::Mutated) => {
                            report.quarantine_entries_removed += 1
                        }
                        Ok(StableMutationOutcome::Missing) => {}
                        Ok(StableMutationOutcome::Changed) | Err(_) => {
                            report.quarantine_failures += 1
                        }
                    }
                }
            }
            continue;
        }

        if cursor.root_entries.is_none() {
            match fs::read_dir(&quarantine_root.path) {
                Ok(entries) => {
                    if !stable_directory_path_is_current(quarantine_root)? {
                        return Err(StorageError::Message(format!(
                            "internal file quarantine changed while its cursor was opened: {}",
                            quarantine_root.path.display()
                        )));
                    }
                    cursor.root_entries = Some(entries);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    report.quarantine_cycle_complete = true;
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let next = cursor
            .root_entries
            .as_mut()
            .expect("quarantine root cursor initialized")
            .next();
        match next {
            Some(Ok(entry)) => {
                remaining -= 1;
                report.quarantine_entries_examined += 1;
                let parent = cursor
                    .root
                    .as_ref()
                    .expect("quarantine root handle initialized")
                    .clone();
                process_quarantine_entry(
                    store,
                    entry,
                    &parent,
                    false,
                    cursor,
                    &mut remaining,
                    report,
                );
            }
            Some(Err(_)) => {
                remaining -= 1;
                report.quarantine_entries_examined += 1;
                report.quarantine_failures += 1;
            }
            None => {
                cursor.root_entries = None;
                report.quarantine_cycle_complete = true;
                break;
            }
        }
    }
    debug_assert!(
        report.quarantine_entries_examined + report.quarantine_mutations_attempted <= budget
    );
    Ok(())
}

fn process_quarantine_entry(
    _store: &SqliteStore,
    entry: fs::DirEntry,
    parent: &StableDirectoryHandle,
    directory_removable: bool,
    cursor: &mut QuarantineDrainCursor,
    remaining: &mut usize,
    report: &mut StorageMaintenanceReport,
) {
    let name = entry.file_name();
    let identity = match relative_entry_identity(parent, &name) {
        Ok(Some(identity)) => identity,
        Ok(None) => return,
        Err(_) => {
            report.quarantine_failures += 1;
            return;
        }
    };
    if !matches!(
        identity.kind,
        InternalEntryKind::File | InternalEntryKind::Directory
    ) {
        report.quarantine_failures += 1;
        return;
    }
    let candidate = DirectoryCandidate { name, identity };
    if candidate.identity.kind == InternalEntryKind::Directory && !directory_removable {
        let directory = match open_stable_directory_candidate(parent, &candidate) {
            Ok(Some(directory)) => directory,
            Ok(None) => return,
            Err(_) => {
                report.quarantine_failures += 1;
                return;
            }
        };
        match fs::read_dir(&directory.path) {
            Ok(entries) => match relative_entry_identity(parent, &candidate.name) {
                Ok(Some(current)) if current == candidate.identity => {
                    cursor.stack.push(QuarantineDirectoryFrame {
                        directory,
                        parent: parent.clone(),
                        name: candidate.name,
                        removable: false,
                        entries,
                    })
                }
                Ok(_) | Err(_) => report.quarantine_failures += 1,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => report.quarantine_failures += 1,
        }
        return;
    }
    let opened = match open_stable_entry(parent, &candidate) {
        Ok(StableEntryOpenOutcome::Opened(opened)) => opened,
        Ok(StableEntryOpenOutcome::Missing) => return,
        Ok(StableEntryOpenOutcome::Changed) | Err(_) => {
            report.quarantine_failures += 1;
            return;
        }
    };
    #[cfg(test)]
    _store.pause_cleanup_test_hook(CleanupTestHookPoint::AfterDrainEntryOpen);
    if opened.identity.kind == InternalEntryKind::Directory {
        let directory = match stable_directory_from_entry(parent, &candidate.name, opened) {
            Ok(directory) => directory,
            Err(_) => {
                report.quarantine_failures += 1;
                return;
            }
        };
        match fs::read_dir(&directory.path) {
            Ok(entries) => match relative_entry_identity(parent, &candidate.name) {
                Ok(Some(current)) if current == candidate.identity => {
                    cursor.stack.push(QuarantineDirectoryFrame {
                        directory,
                        parent: parent.clone(),
                        name: candidate.name,
                        removable: true,
                        entries,
                    })
                }
                Ok(_) | Err(_) => report.quarantine_failures += 1,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => report.quarantine_failures += 1,
        }
        return;
    }

    debug_assert!(*remaining > 0);
    *remaining -= 1;
    report.quarantine_mutations_attempted += 1;
    match delete_stable_entry(parent, &candidate.name, &opened) {
        Ok(StableMutationOutcome::Mutated) => report.quarantine_entries_removed += 1,
        Ok(StableMutationOutcome::Missing) => {}
        Ok(StableMutationOutcome::Changed) | Err(_) => report.quarantine_failures += 1,
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    #[cfg(unix)]
    fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[test]
    fn production_connection_configures_a_busy_timeout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };

        let store = SqliteStore::open(&paths).expect("store");
        let timeout_ms = store
            .connection
            .lock()
            .expect("sqlite mutex poisoned")
            .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
            .expect("busy timeout");

        assert_eq!(timeout_ms, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn second_production_connection_retries_a_busy_writer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let first = SqliteStore::open(&paths).expect("first store");
        first.migrate().expect("migrate");
        let second = SqliteStore::open(&paths).expect("second store");
        first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold writer transaction");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).expect("writer started");
            second
                .connection
                .lock()
                .expect("second sqlite mutex")
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES ('busy-project', 'C:/busy', 'busy', 'none', 1, 1)",
                    [],
                )
        });
        started_rx.recv().expect("writer start");
        std::thread::sleep(Duration::from_millis(100));
        first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .execute_batch("COMMIT")
            .expect("release writer transaction");

        assert_eq!(
            writer.join().expect("writer thread").expect("busy retry"),
            1
        );
        let stored = first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE id = 'busy-project'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stored project");
        assert_eq!(stored, 1);
    }

    #[test]
    fn cleanup_orphan_internal_files_removes_only_unreferenced_appdata_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");

        let harness_root = data_dir.join("harness");
        let referenced_harness = harness_root.join("referenced-run");
        let orphan_harness = harness_root.join("orphan-run");
        fs::create_dir_all(referenced_harness.as_std_path()).expect("referenced harness");
        fs::create_dir_all(orphan_harness.as_std_path()).expect("orphan harness");
        fs::write(referenced_harness.join("event.json").as_std_path(), "{}").expect("write ref");
        fs::write(orphan_harness.join("event.json").as_std_path(), "{}").expect("write orphan");

        fs::create_dir_all(paths.truncation_dir.as_std_path()).expect("truncation dir");
        let referenced_truncation = paths.truncation_dir.join("referenced.txt");
        let orphan_truncation = paths.truncation_dir.join("orphan.txt");
        fs::write(referenced_truncation.as_std_path(), "referenced").expect("write trunc ref");
        fs::write(orphan_truncation.as_std_path(), "orphan").expect("write trunc orphan");

        {
            let connection = store.connection.lock().expect("sqlite mutex poisoned");
            connection
                .execute(
                    "INSERT INTO harness_runs
                     (id, session_id, workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status)
                     VALUES (?1, NULL, ?2, ?3, 'native_runtime', 1, NULL, 'started')",
                    params![
                        "referenced-run",
                        "C:/workspace",
                        referenced_harness.as_str()
                    ],
                )
                .expect("insert harness run");
            connection
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1)",
                    [],
                )
                .expect("insert project");
            connection
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url, created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES ('session', 'project', 'session', 'completed', 'C:/workspace', 'model', 'http://localhost:1234', 1, 1, 1)",
                    [],
                )
                .expect("insert session");
            connection
                .execute(
                    "INSERT INTO protocol_history_items
                     (id, session_id, scope_kind, turn_id, sequence_no, payload_json,
                      payload_sha256, created_at_ms)
                     VALUES (
                         'history', 'session', 'turn', 'turn', 1,
                         json_object(
                             'kind', 'tool_call',
                             'call_id', 'tool',
                             'response_id', 'response',
                             'tool_name', 'read',
                             'arguments_json', '{}'
                         ),
                         'fixture', 1
                     )",
                    [],
                )
                .expect("insert canonical history owner");
            connection
                .execute(
                    "INSERT INTO tool_calls
                     (id, history_item_id, status, truncated_output_path, started_at_ms, finished_at_ms)
                     VALUES ('tool', 'history', 'completed', ?1, 1, 1)",
                    params![referenced_truncation.as_str()],
                )
                .expect("insert tool call");
        }

        let report = store
            .cleanup_orphan_internal_files()
            .expect("cleanup orphan files");

        assert_eq!(report.orphan_harness_dirs_quarantined, 1);
        assert_eq!(report.orphan_truncation_files_quarantined, 1);
        assert_eq!(report.live_quarantine_failures, 0);
        assert_eq!(report.quarantine_failures, 0);
        assert!(
            report.quarantine_entries_examined + report.quarantine_mutations_attempted
                <= INTERNAL_FILE_QUARANTINE_TICK_BUDGET
        );
        assert!(referenced_harness.exists());
        assert!(!orphan_harness.exists());
        assert!(referenced_truncation.exists());
        assert!(!orphan_truncation.exists());
    }

    #[test]
    fn cleanup_batches_are_bounded_and_advance_past_retained_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");

        let harness_root = data_dir.join("harness");
        let retained_count = INTERNAL_FILE_CLEANUP_BATCH_SIZE * 2 + 1;
        {
            let connection = store.connection.lock().expect("sqlite mutex poisoned");
            for index in 0..retained_count {
                let run_id = format!("retained-run-{index:04}");
                let artifact_root = harness_root.join(&run_id);
                fs::create_dir_all(artifact_root.as_std_path()).expect("retained harness dir");
                connection
                    .execute(
                        "INSERT INTO harness_runs
                         (id, session_id, workspace_root, artifact_root, mode,
                          started_at_ms, completed_at_ms, status)
                         VALUES (?1, NULL, 'C:/workspace', ?2, 'native_runtime', 1, NULL, 'started')",
                        params![run_id, artifact_root.as_str()],
                    )
                    .expect("retained harness owner");
            }
        }
        let orphan_harness = harness_root.join("orphan-run");
        fs::create_dir_all(orphan_harness.as_std_path()).expect("orphan harness dir");

        fs::create_dir_all(paths.truncation_dir.as_std_path()).expect("truncation dir");
        for index in 0..retained_count {
            fs::create_dir_all(
                paths
                    .truncation_dir
                    .join(format!("ignored-dir-{index:04}"))
                    .as_std_path(),
            )
            .expect("ignored truncation directory");
        }
        let orphan_truncation = paths.truncation_dir.join("orphan.txt");
        fs::write(orphan_truncation.as_std_path(), "orphan").expect("orphan truncation file");

        let quarantine_root = data_dir.join(INTERNAL_FILE_QUARANTINE_DIR);
        let quarantine_harness_root = quarantine_root.join("harness");
        for index in 0..retained_count {
            let tombstone = quarantine_harness_root.join(format!("tombstone-{index:04}"));
            fs::create_dir_all(tombstone.join("nested").as_std_path())
                .expect("quarantined harness directory");
            fs::write(tombstone.join("nested/event.json").as_std_path(), "{}")
                .expect("quarantined harness file");
        }
        let mut deep_tombstone = quarantine_harness_root.join("deep-tombstone");
        for _ in 0..48 {
            deep_tombstone.push("d");
        }
        fs::create_dir_all(&deep_tombstone).expect("deep quarantined harness tree");
        fs::write(deep_tombstone.join("event.json"), "{}").expect("deep quarantine leaf");

        let mut harness_cycle_complete = false;
        let mut truncation_cycle_complete = false;
        let mut quarantine_cycle_complete = false;
        let mut harness_quarantined = 0;
        let mut truncation_quarantined = 0;
        for tick in 0..24 {
            let report = store
                .cleanup_orphan_internal_files()
                .expect("bounded cleanup batch");
            assert!(report.harness_entries_examined <= INTERNAL_FILE_CLEANUP_BATCH_SIZE);
            assert!(report.truncation_entries_examined <= INTERNAL_FILE_CLEANUP_BATCH_SIZE);
            assert!(
                report.quarantine_entries_examined + report.quarantine_mutations_attempted
                    <= INTERNAL_FILE_QUARANTINE_TICK_BUDGET
            );
            harness_cycle_complete |= report.harness_cycle_complete;
            truncation_cycle_complete |= report.truncation_cycle_complete;
            quarantine_cycle_complete |= report.quarantine_cycle_complete;
            harness_quarantined += report.orphan_harness_dirs_quarantined;
            truncation_quarantined += report.orphan_truncation_files_quarantined;
            assert_eq!(report.live_quarantine_failures, 0);
            assert_eq!(report.quarantine_failures, 0);
            if tick == 0 {
                assert!(
                    quarantine_harness_root.exists()
                        && fs::read_dir(quarantine_harness_root.as_std_path())
                            .expect("partially drained quarantine")
                            .next()
                            .is_some(),
                    "one bounded tick must not recursively drain an oversized quarantine"
                );
            }
            if harness_cycle_complete && truncation_cycle_complete && quarantine_cycle_complete {
                break;
            }
        }

        assert!(
            harness_cycle_complete,
            "the retained head must not starve the rest of the harness directory"
        );
        assert!(
            truncation_cycle_complete,
            "the retained head must not starve the rest of the truncation directory"
        );
        assert!(
            quarantine_cycle_complete,
            "the quarantine drain must advance through more than one batch"
        );
        assert_eq!(harness_quarantined, 1);
        assert_eq!(truncation_quarantined, 1);
        assert!(!orphan_harness.exists());
        assert!(!orphan_truncation.exists());
        if quarantine_harness_root.exists() {
            assert!(
                fs::read_dir(quarantine_harness_root.as_std_path())
                    .expect("harness quarantine root")
                    .next()
                    .is_none(),
                "all harness tombstones in the completed quarantine cycle must be drained"
            );
        }
        assert!(harness_root.join("retained-run-0000").exists());
    }

    #[test]
    fn cleanup_restart_rescans_remaining_live_entries_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let harness_root = data_dir.join("harness");
        for index in 0..(INTERNAL_FILE_ROOT_BATCH_SIZE + 8) {
            fs::create_dir_all(harness_root.join(format!("orphan-{index:04}")))
                .expect("orphan harness directory");
        }

        let first = store
            .cleanup_orphan_internal_files()
            .expect("first bounded cleanup");
        assert_eq!(
            first.harness_entries_examined,
            INTERNAL_FILE_ROOT_BATCH_SIZE
        );
        assert!(!first.harness_cycle_complete);
        assert!(
            fs::read_dir(&harness_root)
                .expect("remaining live root")
                .next()
                .is_some()
        );
        drop(store);

        let restarted = SqliteStore::open(&paths).expect("restarted store");
        restarted.migrate().expect("restarted migration");
        for _ in 0..8 {
            let report = restarted
                .cleanup_orphan_internal_files()
                .expect("restart cleanup tick");
            assert!(
                report.quarantine_entries_examined + report.quarantine_mutations_attempted
                    <= INTERNAL_FILE_QUARANTINE_TICK_BUDGET
            );
            if !harness_root.exists()
                || fs::read_dir(&harness_root)
                    .expect("live harness root")
                    .next()
                    .is_none()
            {
                break;
            }
        }
        assert!(
            !harness_root.exists()
                || fs::read_dir(&harness_root)
                    .expect("live harness root")
                    .next()
                    .is_none()
        );
    }

    #[test]
    fn cleanup_rejects_a_linked_live_root_without_touching_its_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let external = temp.path().join("external-live-root");
        fs::create_dir(&external).expect("external directory");
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, "keep").expect("external sentinel");
        if create_directory_symlink(&external, data_dir.join("harness").as_std_path()).is_err() {
            return;
        }

        assert!(store.cleanup_orphan_internal_files().is_err());
        assert_eq!(
            fs::read_to_string(sentinel).expect("untouched sentinel"),
            "keep"
        );
    }

    #[test]
    fn cleanup_rejects_a_linked_quarantine_namespace_without_traversing_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let quarantine_root = data_dir.join(INTERNAL_FILE_QUARANTINE_DIR);
        fs::create_dir(&quarantine_root).expect("quarantine root");
        let external = temp.path().join("external-quarantine");
        fs::create_dir(&external).expect("external directory");
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, "keep").expect("external sentinel");
        if create_directory_symlink(&external, quarantine_root.join("harness").as_std_path())
            .is_err()
        {
            return;
        }

        assert!(store.cleanup_orphan_internal_files().is_err());
        assert_eq!(
            fs::read_to_string(sentinel).expect("untouched sentinel"),
            "keep"
        );
    }

    #[test]
    fn cleanup_root_replacement_race_cannot_redirect_live_quarantine() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let harness_root = data_dir.join("harness");
        let orphan = harness_root.join("race-run");
        fs::create_dir_all(orphan.as_std_path()).expect("orphan harness");
        fs::write(orphan.join("event.json").as_std_path(), "{}").expect("orphan event");
        let external = temp.path().join("external-live-race");
        fs::create_dir(&external).expect("external directory");
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, "keep").expect("external sentinel");
        let parked = data_dir.join("harness-parked");

        let (reached, resume) =
            store.install_cleanup_test_hook(CleanupTestHookPoint::BeforeLiveEntryOpen);
        let cleanup_store = store.clone();
        let cleanup = std::thread::spawn(move || cleanup_store.cleanup_orphan_internal_files());
        reached
            .recv_timeout(Duration::from_secs(5))
            .expect("live root pin reached");
        let replacement = fs::rename(harness_root.as_std_path(), parked.as_std_path());
        #[cfg(windows)]
        assert!(
            replacement.is_err(),
            "the no-share-delete live root handle must reject replacement"
        );
        #[cfg(unix)]
        {
            replacement.expect("move pinned Unix root name");
            create_directory_symlink(&external, harness_root.as_std_path())
                .expect("install external root replacement");
        }
        resume.send(()).expect("resume cleanup");
        let report = cleanup
            .join()
            .expect("cleanup thread")
            .expect("root race cleanup");

        assert_eq!(
            fs::read_to_string(&sentinel).expect("untouched external sentinel"),
            "keep"
        );
        assert_eq!(report.orphan_harness_dirs_quarantined, 1);
        assert_eq!(report.live_quarantine_failures, 0);
    }

    #[test]
    fn cleanup_entry_replacement_race_cannot_rename_an_external_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let candidate = paths.truncation_dir.join("race-output.txt");
        let parked = paths.truncation_dir.join("race-output-parked.txt");
        fs::write(candidate.as_std_path(), "internal").expect("race candidate");
        let sentinel = temp.path().join("external-file-sentinel.txt");
        fs::write(&sentinel, "keep").expect("external sentinel");

        let (reached, resume) =
            store.install_cleanup_test_hook(CleanupTestHookPoint::AfterLiveEntryOpen);
        let cleanup_store = store.clone();
        let cleanup = std::thread::spawn(move || cleanup_store.cleanup_orphan_internal_files());
        reached
            .recv_timeout(Duration::from_secs(5))
            .expect("live entry handle reached");
        let replacement = fs::rename(candidate.as_std_path(), parked.as_std_path());
        #[cfg(windows)]
        assert!(
            replacement.is_err(),
            "the no-share-delete entry handle must reject replacement"
        );
        #[cfg(unix)]
        {
            replacement.expect("move opened Unix candidate name");
            std::os::unix::fs::symlink(&sentinel, candidate.as_std_path())
                .expect("install external file replacement");
        }
        resume.send(()).expect("resume cleanup");
        let report = cleanup
            .join()
            .expect("cleanup thread")
            .expect("entry race cleanup");

        assert_eq!(
            fs::read_to_string(&sentinel).expect("untouched external sentinel"),
            "keep"
        );
        #[cfg(windows)]
        {
            assert_eq!(report.orphan_truncation_files_quarantined, 1);
            assert_eq!(report.live_quarantine_failures, 0);
        }
        #[cfg(unix)]
        {
            assert_eq!(report.orphan_truncation_files_quarantined, 0);
            assert_eq!(report.live_quarantine_failures, 1);
            assert_eq!(
                fs::read_to_string(parked).expect("parked candidate"),
                "internal"
            );
        }
    }

    #[test]
    fn cleanup_drain_entry_replacement_race_cannot_delete_an_external_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let namespace = data_dir.join(INTERNAL_FILE_QUARANTINE_DIR).join("harness");
        let candidate = namespace.join("race-tombstone");
        let parked = namespace.join("race-tombstone-parked");
        fs::create_dir_all(candidate.as_std_path()).expect("quarantine tombstone");
        let external = temp.path().join("external-drain-race");
        fs::create_dir(&external).expect("external directory");
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, "keep").expect("external sentinel");

        let (reached, resume) =
            store.install_cleanup_test_hook(CleanupTestHookPoint::AfterDrainEntryOpen);
        let cleanup_store = store.clone();
        let cleanup = std::thread::spawn(move || cleanup_store.cleanup_orphan_internal_files());
        reached
            .recv_timeout(Duration::from_secs(5))
            .expect("drain entry handle reached");
        let replacement = fs::rename(candidate.as_std_path(), parked.as_std_path());
        #[cfg(windows)]
        assert!(
            replacement.is_err(),
            "the no-share-delete drain handle must reject replacement"
        );
        #[cfg(unix)]
        {
            replacement.expect("move opened Unix tombstone name");
            create_directory_symlink(&external, candidate.as_std_path())
                .expect("install external drain replacement");
        }
        resume.send(()).expect("resume cleanup");
        let report = cleanup
            .join()
            .expect("cleanup thread")
            .expect("drain race cleanup");

        assert_eq!(
            fs::read_to_string(&sentinel).expect("untouched external sentinel"),
            "keep"
        );
        assert!(
            report.quarantine_entries_examined + report.quarantine_mutations_attempted
                <= INTERNAL_FILE_QUARANTINE_TICK_BUDGET
        );
        #[cfg(windows)]
        assert_eq!(report.quarantine_failures, 0);
        #[cfg(unix)]
        assert!(report.quarantine_failures >= 1);
    }

    #[test]
    fn harness_run_id_does_not_own_a_different_artifact_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");

        let candidate = data_dir.join("harness").join("same-run-id");
        fs::create_dir_all(candidate.as_std_path()).expect("candidate harness dir");
        store
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode,
                  started_at_ms, completed_at_ms, status)
                 VALUES ('same-run-id', NULL, 'C:/workspace', ?1,
                         'native_runtime', 1, NULL, 'started')",
                params![data_dir.join("elsewhere/same-run-id").as_str()],
            )
            .expect("different artifact root owner");

        let report = store
            .cleanup_orphan_internal_files()
            .expect("exact-owner cleanup");

        assert_eq!(report.orphan_harness_dirs_quarantined, 1);
        assert_eq!(report.live_quarantine_failures, 0);
        assert!(!candidate.exists());
    }

    #[test]
    fn cleanup_waits_until_a_new_internal_file_has_a_durable_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let producer =
            crate::storage::InternalFileProducerLease::acquire(&paths).expect("producer lease");
        let harness_dir = data_dir.join("harness").join("new-run");
        fs::create_dir_all(harness_dir.as_std_path()).expect("new harness dir");
        fs::write(harness_dir.join("event.json").as_std_path(), "{}").expect("new harness file");

        let cleanup_store = store.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            started_tx.send(()).expect("cleanup started");
            let result = cleanup_store.cleanup_orphan_internal_files();
            finished_tx.send(result).expect("cleanup finished");
        });
        started_rx.recv().expect("cleanup start signal");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "cleanup must wait while the producer owns the shared file fence"
        );

        store
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "INSERT INTO harness_runs
                 (id, session_id, workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status)
                 VALUES ('new-run', NULL, 'C:/workspace', ?1, 'native_runtime', 1, NULL, 'started')",
                params![harness_dir.as_str()],
            )
            .expect("commit durable harness owner");
        drop(producer);

        let report = finished_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cleanup unblocked")
            .expect("cleanup succeeds");
        cleanup.join().expect("cleanup thread");
        assert_eq!(report.orphan_harness_dirs_quarantined, 0);
        assert!(harness_dir.exists());
    }

    #[test]
    fn cleanup_waits_until_a_new_truncation_file_has_a_durable_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let producer =
            crate::storage::InternalFileProducerLease::acquire(&paths).expect("producer lease");
        let truncation_file = paths.truncation_dir.join("new-output.txt");
        fs::write(truncation_file.as_std_path(), "new output").expect("new truncation file");

        let cleanup_store = store.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            started_tx.send(()).expect("cleanup started");
            let result = cleanup_store.cleanup_orphan_internal_files();
            finished_tx.send(result).expect("cleanup finished");
        });
        started_rx.recv().expect("cleanup start signal");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "cleanup must wait while the truncation producer owns the shared file fence"
        );

        {
            let connection = store.connection.lock().expect("sqlite mutex");
            connection
                .execute_batch(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1);
                     INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url,
                      created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES ('session', 'project', 'session', 'completed', 'C:/workspace',
                             'model', 'http://localhost:1234', 1, 1, 1);
                     INSERT INTO protocol_history_items
                     (id, session_id, scope_kind, turn_id, sequence_no, payload_json,
                      payload_sha256, created_at_ms)
                     VALUES (
                         'history', 'session', 'turn', 'turn', 1,
                         json_object(
                             'kind', 'tool_call',
                             'call_id', 'tool',
                             'response_id', 'response',
                             'tool_name', 'read',
                             'arguments_json', '{}'
                         ),
                         'fixture', 1
                     );",
                )
                .expect("canonical truncation owner fixture");
            connection
                .execute(
                    "INSERT INTO tool_calls
                     (id, history_item_id, status, truncated_output_path,
                      started_at_ms, finished_at_ms)
                     VALUES ('tool', 'history', 'completed', ?1, 1, 1)",
                    params![truncation_file.as_str()],
                )
                .expect("commit durable truncation owner");
        }
        drop(producer);

        let report = finished_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("cleanup unblocked")
            .expect("cleanup succeeds");
        cleanup.join().expect("cleanup thread");
        assert_eq!(report.orphan_truncation_files_quarantined, 0);
        assert!(truncation_file.exists());
    }
}
