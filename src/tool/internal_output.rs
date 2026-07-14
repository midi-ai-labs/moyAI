use camino::{Utf8Path, Utf8PathBuf};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::workspace::{AccessKind, GuardedPath, PathGuard};

pub(crate) async fn resolve_path(
    ctx: &ToolContext<'_>,
    requested: &Utf8Path,
    access: AccessKind,
) -> Result<GuardedPath, ToolError> {
    match PathGuard::require_path(ctx.workspace, requested, access) {
        Ok(guarded) => Ok(guarded),
        Err(boundary_error) => {
            let absolute =
                crate::workspace::project::normalize_path(&ctx.workspace.cwd, requested)?;
            if !matches!(access, AccessKind::Read | AccessKind::Search)
                || !is_internal_truncation_path(
                    &absolute,
                    &ctx.services.storage_paths.truncation_dir,
                )?
                || !ctx
                    .services
                    .store
                    .session_repo()
                    .session_owns_truncated_output(ctx.session.session.id, &absolute)
                    .await?
            {
                return Err(boundary_error.into());
            }
            Ok(GuardedPath {
                absolute: absolute.clone(),
                relative_to_root: absolute,
                inside_workspace: false,
                trusted_external: true,
            })
        }
    }
}

fn is_internal_truncation_path(
    path: &Utf8Path,
    truncation_dir: &Utf8Path,
) -> Result<bool, ToolError> {
    if !path.starts_with(truncation_dir) || !path.exists() || !truncation_dir.exists() {
        return Ok(false);
    }
    let canonical_path =
        Utf8PathBuf::from_path_buf(std::fs::canonicalize(path)?).map_err(|path| {
            ToolError::Message(format!("path `{}` is not valid UTF-8", path.display()))
        })?;
    let canonical_dir = Utf8PathBuf::from_path_buf(std::fs::canonicalize(truncation_dir)?)
        .map_err(|path| {
            ToolError::Message(format!("path `{}` is not valid UTF-8", path.display()))
        })?;
    Ok(canonical_path.starts_with(canonical_dir))
}
