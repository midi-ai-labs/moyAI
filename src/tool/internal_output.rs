use camino::{Utf8Path, Utf8PathBuf};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::workspace::{AccessKind, GuardedPath, PathGuard};

#[must_use = "consume the resolved path after permission confirmation"]
pub(crate) struct ResolvedPath(ResolvedPathKind);

enum ResolvedPathKind {
    Normal(GuardedPath),
    InternalTruncation(OpenedInternalPath),
}

struct OpenedInternalPath {
    guarded: GuardedPath,
    file: std::fs::File,
}

pub(crate) struct ResolvedPathPermission<'a> {
    pub(crate) absolute: &'a Utf8Path,
    pub(crate) inside_workspace: bool,
    pub(crate) trusted_external: bool,
}

pub(crate) struct ResolvedReadFile {
    absolute: Utf8PathBuf,
    relative_to_root: Utf8PathBuf,
    inside_workspace: bool,
    file: std::fs::File,
}

pub(crate) enum ResolvedSearchPath {
    Normal(GuardedPath),
    Internal(InternalSearchFile),
}

pub(crate) struct InternalSearchFile {
    absolute: Utf8PathBuf,
    file: std::fs::File,
}

impl ResolvedPath {
    pub(crate) fn permission(&self) -> ResolvedPathPermission<'_> {
        let guarded = match &self.0 {
            ResolvedPathKind::Normal(guarded) => guarded,
            ResolvedPathKind::InternalTruncation(opened) => &opened.guarded,
        };
        ResolvedPathPermission {
            absolute: &guarded.absolute,
            inside_workspace: guarded.inside_workspace,
            trusted_external: guarded.trusted_external,
        }
    }

    pub(crate) fn into_read_file(self) -> Result<ResolvedReadFile, ToolError> {
        match self.0 {
            ResolvedPathKind::Normal(guarded) => {
                let file = PathGuard::open_validated_metadata_handle(&guarded)?;
                Ok(ResolvedReadFile::from_guarded_file(guarded, file))
            }
            ResolvedPathKind::InternalTruncation(opened) => {
                let (guarded, file) = opened.into_parts();
                Ok(ResolvedReadFile::from_guarded_file(guarded, file))
            }
        }
    }

    pub(crate) fn into_search_path(self) -> ResolvedSearchPath {
        match self.0 {
            ResolvedPathKind::Normal(guarded) => ResolvedSearchPath::Normal(guarded),
            ResolvedPathKind::InternalTruncation(opened) => {
                let (guarded, file) = opened.into_parts();
                ResolvedSearchPath::Internal(InternalSearchFile {
                    absolute: guarded.absolute,
                    file,
                })
            }
        }
    }
}

impl ResolvedReadFile {
    fn from_guarded_file(guarded: GuardedPath, file: std::fs::File) -> Self {
        Self {
            absolute: guarded.absolute,
            relative_to_root: guarded.relative_to_root,
            inside_workspace: guarded.inside_workspace,
            file,
        }
    }

    pub(crate) fn absolute(&self) -> &Utf8Path {
        &self.absolute
    }

    pub(crate) fn relative_to_root(&self) -> &Utf8Path {
        &self.relative_to_root
    }

    pub(crate) const fn inside_workspace(&self) -> bool {
        self.inside_workspace
    }

    pub(crate) fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    pub(crate) fn with_file<T>(
        &mut self,
        operation: impl FnOnce(&mut std::fs::File) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        operation(&mut self.file)
    }
}

impl InternalSearchFile {
    pub(crate) fn into_parts(self) -> (Utf8PathBuf, std::fs::File) {
        (self.absolute, self.file)
    }
}

impl OpenedInternalPath {
    fn into_parts(self) -> (GuardedPath, std::fs::File) {
        (self.guarded, self.file)
    }
}

pub(crate) async fn resolve_path(
    ctx: &ToolContext<'_>,
    requested: &Utf8Path,
    access: AccessKind,
) -> Result<ResolvedPath, ToolError> {
    match PathGuard::require_path(ctx.workspace, requested, access) {
        Ok(guarded) => Ok(ResolvedPath(ResolvedPathKind::Normal(guarded))),
        Err(boundary_error) => {
            let absolute =
                crate::workspace::project::normalize_path(&ctx.workspace.cwd, requested)?;
            if !matches!(access, AccessKind::Read | AccessKind::Search) {
                return Err(boundary_error.into());
            }

            let Some((opened, owner_lookup_path)) = open_internal_truncation_path(
                &absolute,
                &ctx.services.storage_paths.truncation_dir,
            )?
            else {
                return Err(boundary_error.into());
            };
            if !ctx
                .services
                .store
                .session_repo()
                .session_owns_truncated_output(ctx.session.session.id, &owner_lookup_path)
                .await?
            {
                return Err(boundary_error.into());
            }

            Ok(ResolvedPath(ResolvedPathKind::InternalTruncation(opened)))
        }
    }
}

fn open_internal_truncation_path(
    path: &Utf8Path,
    truncation_dir: &Utf8Path,
) -> Result<Option<(OpenedInternalPath, Utf8PathBuf)>, ToolError> {
    let guarded = match PathGuard::trusted_internal_path(path, truncation_dir) {
        Ok(guarded) => guarded,
        Err(_) => return Ok(None),
    };
    let file = PathGuard::open_validated_read_file(&guarded)?;
    let owner_lookup_path = PathGuard::opened_file_identity_path(&file)?;
    if !PathGuard::same_existing_namespace_entry(path, &owner_lookup_path)? {
        return Ok(None);
    }
    Ok(Some((
        OpenedInternalPath { guarded, file },
        owner_lookup_path,
    )))
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use camino::Utf8PathBuf;

    use super::{ResolvedPath, ResolvedPathKind, open_internal_truncation_path};
    use crate::workspace::PathGuard;

    #[cfg(unix)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("redirect fixture");
    }

    #[cfg(windows)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::windows::fs::symlink_file(target, link).expect("redirect fixture");
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

    #[test]
    fn internal_output_resolution_preserves_an_exact_owned_path_and_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let owned_path = truncation_dir.join("owned.txt");
        std::fs::write(&owned_path, "owned output").expect("owned fixture");

        let (opened, owner_lookup_path) =
            open_internal_truncation_path(&owned_path, &truncation_dir)
                .expect("typed guard result")
                .expect("exact internal output path");
        assert_eq!(owner_lookup_path, owned_path);
        let (guarded, file) = opened.into_parts();

        PathGuard::validate_open_file(&guarded, &file).expect("stable owned output handle");
    }

    #[test]
    fn normal_read_resolution_reaches_directory_metadata_through_a_stable_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let directory = root.join("directory");
        std::fs::create_dir(&directory).expect("directory fixture");
        let guarded = PathGuard::trusted_internal_path(&directory, &root).expect("guard directory");
        let resolved = ResolvedPath(ResolvedPathKind::Normal(guarded));

        let opened = resolved
            .into_read_file()
            .expect("open stable file-or-directory handle");

        assert!(opened.metadata().expect("handle metadata").is_dir());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn internal_output_guard_rejects_a_symlink_redirect_to_an_owned_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let owned_path = truncation_dir.join("owned.txt");
        let owned_target_path = truncation_dir.join("owned-target.txt");
        std::fs::write(&owned_path, "owned alias row").expect("owned alias fixture");
        std::fs::write(&owned_target_path, "owned target row").expect("owned target fixture");
        std::fs::remove_file(&owned_path).expect("replace owned fixture");
        link_file(&owned_target_path, &owned_path);

        let direct_target = open_internal_truncation_path(&owned_target_path, &truncation_dir)
            .expect("direct target open")
            .expect("the target is itself a valid internal candidate");
        assert_eq!(direct_target.1, owned_target_path);
        let opened =
            open_internal_truncation_path(&owned_path, &truncation_dir).expect("typed open result");

        assert!(
            opened.is_none(),
            "a lexical path must not redirect to another session-owned internal output"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn replacement_before_owner_query_keeps_the_opened_object_and_original_lookup_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let owned_path = truncation_dir.join("owned.txt");
        std::fs::write(&owned_path, "owned output").expect("owned fixture");
        let detached_path = truncation_dir.join("detached.txt");
        let (opened, owner_lookup_path) =
            open_internal_truncation_path(&owned_path, &truncation_dir)
                .expect("typed open result")
                .expect("opened owned output");

        std::fs::rename(&owned_path, &detached_path).expect("rename after held open");
        std::fs::write(&owned_path, "replacement output").expect("same-name replacement");
        assert_eq!(
            owner_lookup_path, owned_path,
            "the exact DB key must already come from the held handle before the query"
        );
        let resolved = ResolvedPath(ResolvedPathKind::InternalTruncation(opened));
        let mut resolved = resolved.into_read_file().expect("consume held read handle");
        let mut content = String::new();
        resolved
            .with_file(|file| file.read_to_string(&mut content))
            .expect("read the already-authorized handle");

        assert_eq!(content, "owned output");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn replacement_after_owner_query_keeps_the_authorized_opened_object() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let owned_path = truncation_dir.join("owned.txt");
        let detached_path = truncation_dir.join("detached.txt");
        std::fs::write(&owned_path, "owned output").expect("owned fixture");
        let (opened, owner_lookup_path) =
            open_internal_truncation_path(&owned_path, &truncation_dir)
                .expect("typed open result")
                .expect("opened owned output");

        assert_eq!(
            owner_lookup_path, owned_path,
            "simulate the successful exact indexed owner query"
        );
        std::fs::rename(&owned_path, &detached_path).expect("rename after owner query");
        std::fs::write(&owned_path, "replacement output").expect("same-name replacement");
        let (_, mut file) = opened.into_parts();
        let mut content = String::new();
        file.read_to_string(&mut content)
            .expect("read the already-authorized handle");

        assert_eq!(content, "owned output");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn hardlink_alias_does_not_inherit_its_peers_exact_owner_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let owned_path = truncation_dir.join("owned.txt");
        let alias_path = truncation_dir.join("alias.txt");
        std::fs::write(&owned_path, "owned output").expect("owned fixture");
        std::fs::hard_link(&owned_path, &alias_path).expect("hardlink alias fixture");

        let authorized_as_owned_peer = open_internal_truncation_path(&alias_path, &truncation_dir)
            .expect("typed open result")
            .filter(|(_, owner_lookup_path)| owner_lookup_path == &owned_path);

        assert!(
            authorized_as_owned_peer.is_none(),
            "an exact owner row for one hardlink name must not authorize another name"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_owner_lookup_path_remains_case_sensitive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let upper_path = truncation_dir.join("A.txt");
        let lower_path = truncation_dir.join("a.txt");
        std::fs::write(&upper_path, "upper owner").expect("upper fixture");
        std::fs::write(&lower_path, "lower non-owner").expect("lower fixture");

        let (_, upper_lookup) = open_internal_truncation_path(&upper_path, &truncation_dir)
            .expect("upper open")
            .expect("upper candidate");
        let lower_authorized_as_upper = open_internal_truncation_path(&lower_path, &truncation_dir)
            .expect("lower open")
            .filter(|(_, owner_lookup_path)| owner_lookup_path == &upper_lookup);

        assert_eq!(upper_lookup, upper_path);
        assert!(
            lower_authorized_as_upper.is_none(),
            "the exact owner query must not fold a case-sensitive namespace"
        );
    }

    #[cfg(windows)]
    #[test]
    fn exact_owner_lookup_path_respects_windows_case_sensitive_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        std::fs::create_dir(&truncation_dir).expect("empty truncation directory");
        enable_case_sensitive_directory(&truncation_dir);
        let upper_path = truncation_dir.join("A.txt");
        let lower_path = truncation_dir.join("a.txt");
        std::fs::write(&upper_path, "upper owner").expect("upper fixture");
        std::fs::write(&lower_path, "lower non-owner").expect("lower fixture");

        let (_, upper_lookup) = open_internal_truncation_path(&upper_path, &truncation_dir)
            .expect("upper open")
            .expect("upper candidate");
        let (_, lower_lookup) = open_internal_truncation_path(&lower_path, &truncation_dir)
            .expect("lower open")
            .expect("lower candidate");

        assert_eq!(upper_lookup, upper_path);
        assert_eq!(lower_lookup, lower_path);
        assert_ne!(upper_lookup, lower_lookup);
        assert!(
            !PathGuard::same_existing_namespace_entry(&upper_path, &lower_path)
                .expect("case-sensitive namespace comparison")
        );
    }

    #[cfg(windows)]
    #[test]
    fn internal_output_guard_accepts_windows_case_and_extended_identity_aliases() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("Truncated")).expect("utf8 path");
        std::fs::create_dir_all(&truncation_dir).expect("truncation directory");
        let output_path = truncation_dir.join("Output.txt");
        std::fs::write(&output_path, "owned output").expect("output fixture");

        let case_variant = Utf8PathBuf::from(output_path.as_str().to_ascii_uppercase());
        let (case_opened, case_owner_lookup) =
            open_internal_truncation_path(&case_variant, &truncation_dir)
                .expect("case open")
                .expect("same Windows namespace entry");
        assert_eq!(case_owner_lookup, output_path);
        let (case_guard, case_file) = case_opened.into_parts();
        PathGuard::validate_open_file(&case_guard, &case_file).expect("case-variant stable open");

        let extended = Utf8PathBuf::from_path_buf(
            std::fs::canonicalize(&output_path).expect("canonical output path"),
        )
        .expect("utf8 canonical output path");
        let (extended_opened, extended_owner_lookup) =
            open_internal_truncation_path(&extended, &truncation_dir)
                .expect("extended open")
                .expect("same extended Windows namespace entry");
        assert_eq!(extended_owner_lookup, output_path);
        let (extended_guard, extended_file) = extended_opened.into_parts();
        PathGuard::validate_open_file(&extended_guard, &extended_file)
            .expect("extended stable open");
    }
}
