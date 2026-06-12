use camino::{Utf8Path, Utf8PathBuf};

use crate::config::ResolvedConfig;
use crate::error::WorkspaceError;
use crate::session::ProjectId;
use crate::workspace::ignore::IgnorePlan;
use crate::workspace::path_guard::PathPolicy;
use crate::workspace::project::{VcsKind, find_workspace_root};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    pub project_id: ProjectId,
    pub root: Utf8PathBuf,
    pub cwd: Utf8PathBuf,
    pub vcs: VcsKind,
    pub ignore: IgnorePlan,
    pub protected_paths: Vec<Utf8PathBuf>,
    pub path_policy: PathPolicy,
}

pub struct WorkspaceDiscovery;

impl WorkspaceDiscovery {
    pub fn discover(
        start_dir: &Utf8Path,
        config: &ResolvedConfig,
    ) -> Result<Workspace, WorkspaceError> {
        let cwd = absolute_start_dir(start_dir)?;
        let root = find_workspace_root(&cwd)?.unwrap_or_else(|| cwd.clone());
        Ok(workspace_from_cwd_and_root(cwd, root, config))
    }

    pub fn discover_fixed_root(
        start_dir: &Utf8Path,
        config: &ResolvedConfig,
    ) -> Result<Workspace, WorkspaceError> {
        let cwd = absolute_start_dir(start_dir)?;
        Ok(workspace_from_cwd_and_root(cwd.clone(), cwd, config))
    }
}

fn absolute_start_dir(start_dir: &Utf8Path) -> Result<Utf8PathBuf, WorkspaceError> {
    if start_dir.is_absolute() {
        return crate::workspace::project::normalize_path(start_dir, Utf8Path::new("."));
    }
    let current =
        std::env::current_dir().map_err(|error| WorkspaceError::Message(error.to_string()))?;
    let current = Utf8PathBuf::from_path_buf(current)
        .map_err(|_| WorkspaceError::Message("current directory is not valid UTF-8".to_string()))?;
    crate::workspace::project::normalize_path(&current, start_dir)
}

fn workspace_from_cwd_and_root(
    cwd: Utf8PathBuf,
    root: Utf8PathBuf,
    config: &ResolvedConfig,
) -> Workspace {
    let vcs = if root.join(".git").exists() {
        VcsKind::Git
    } else {
        VcsKind::None
    };
    let project_id = ProjectId::from_stable_input(root.as_str());
    let ignore = IgnorePlan::default_with(config.workspace.extra_ignore_globs.clone());

    let mut protected_paths = default_protected_paths();
    protected_paths.extend(config.workspace.protected_paths.iter().cloned());
    protected_paths.sort();
    protected_paths.dedup();

    let path_policy = PathPolicy {
        workspace_root: root.clone(),
        additional_read_roots: config.permissions.additional_read_roots.clone(),
        additional_write_roots: config.permissions.additional_write_roots.clone(),
    };

    Workspace {
        project_id,
        root,
        cwd,
        vcs,
        ignore,
        protected_paths,
        path_policy,
    }
}

fn default_protected_paths() -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();

    if cfg!(windows) {
        for key in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(value) = std::env::var(key) {
                let path = Utf8PathBuf::from(value);
                if path.exists() {
                    paths.push(path);
                }
            }
        }
    } else {
        for value in ["/bin", "/etc", "/usr", "/sbin", "/var"] {
            let path = Utf8PathBuf::from(value);
            if path.exists() {
                paths.push(path);
            }
        }
    }

    paths
}
