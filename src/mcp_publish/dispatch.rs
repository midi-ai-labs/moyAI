//! Fixed-target, read-only MCP execution through the existing tool registry.
//!
//! Authentication and profile-generation revocation belong to the serving owner. This
//! dispatcher never follows Desktop selection, borrows a human turn, or creates one.

use std::fs::{File, OpenOptions};
use std::sync::Arc;

use async_trait::async_trait;
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PublishAuthentication, PublishProfile, PublishTarget};
use crate::config::ResolvedConfig;
use crate::runtime::RunControl;
use crate::session::{ProjectId, ProjectRepository, SessionId, SessionRecord, SessionRepository};
use crate::storage::StoreBundle;
use crate::tool::ToolName;
use crate::tool::read_context::ReadToolContext;
use crate::tool::registry::ToolRegistry;
use crate::workspace::path_guard::ExistingObjectIdentity;
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace, WorkspaceDiscovery};

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PublishCallError {
    #[error("MCP publish target is invalid")]
    InvalidTarget,
    #[error("MCP publish target changed; stop and configure the profile again")]
    TargetChanged,
    #[error("MCP tool is not published by this profile")]
    ToolUnavailable,
    #[error("MCP tool arguments are invalid")]
    InvalidArguments,
    #[error("MCP operation was cancelled")]
    Cancelled,
    #[error("MCP operation is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait PublishToolDispatcher: Send + Sync {
    fn tool_descriptors(&self) -> Vec<Value>;
    async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value, PublishCallError>;

    /// Only the serving transport supplies this context after mTLS and current
    /// Hub policy introspection. JSON tool arguments can never create it.
    async fn call_authorized(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
        authority: crate::device_network::VerifiedGrant,
    ) -> Result<Value, PublishCallError> {
        let _ = (name, arguments, cancel, authority);
        Err(PublishCallError::Unavailable)
    }
}

#[async_trait]
pub trait DeviceRequestAuthenticator: Send + Sync {
    async fn authenticate(
        &self,
        token: &str,
        actor_certificate_sha256: &str,
        action: &str,
    ) -> Result<crate::device_network::VerifiedGrant, PublishCallError>;
}

#[derive(Clone)]
pub struct PublishReadDispatcher {
    inner: Arc<ReadDispatcherInner>,
}

struct ReadDispatcherInner {
    profile: PublishProfile,
    store: StoreBundle,
    config: ResolvedConfig,
    registry: ToolRegistry,
    target: TargetSnapshot,
}

pub(crate) enum TargetSnapshot {
    Temp,
    Workspace(WorkspaceTargetSnapshot),
}

pub(crate) struct WorkspaceTargetSnapshot {
    workspace: Workspace,
    project_root: Utf8PathBuf,
    project_created_at_ms: i64,
    legacy_session_created_at_ms: Option<i64>,
    workspace_pin: DirectoryPin,
    project_pin: DirectoryPin,
}

struct DirectoryPin {
    guarded: GuardedPath,
    identity: ExistingObjectIdentity,
    _file: File,
}

impl DirectoryPin {
    fn open(guarded: GuardedPath) -> Result<Self, PublishCallError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(&guarded.absolute)
            .map_err(|_| PublishCallError::InvalidTarget)?;
        if !file
            .metadata()
            .map_err(|_| PublishCallError::InvalidTarget)?
            .is_dir()
        {
            return Err(PublishCallError::InvalidTarget);
        }
        PathGuard::validate_open_file(&guarded, &file)
            .map_err(|_| PublishCallError::InvalidTarget)?;
        let identity = PathGuard::opened_object_identity(&file)
            .map_err(|_| PublishCallError::InvalidTarget)?;
        Ok(Self {
            guarded,
            identity,
            _file: file,
        })
    }

    fn verify(&self) -> Result<(), PublishCallError> {
        PathGuard::revalidate(&self.guarded).map_err(|_| PublishCallError::TargetChanged)?;
        let current =
            Self::open(self.guarded.clone()).map_err(|_| PublishCallError::TargetChanged)?;
        if current.identity != self.identity {
            return Err(PublishCallError::TargetChanged);
        }
        Ok(())
    }
}

fn read_config(
    source: &ResolvedConfig,
    protected_roots: &[Utf8PathBuf],
    store: &StoreBundle,
) -> ResolvedConfig {
    // Do not copy provider credentials, additional roots, Full Access, MCP clients or
    // model instructions into the external caller's context.
    let mut config = ResolvedConfig::default();
    config.workspace = source.workspace.clone();
    config
        .workspace
        .protected_paths
        .extend(protected_roots.iter().cloned());
    config
        .workspace
        .protected_paths
        .push(store.paths().data_dir.clone());
    config.file_guard = source.file_guard.clone();
    config.file_guard.max_inline_read_bytes =
        config.file_guard.max_inline_read_bytes.min(8 * 1024 * 1024);
    config.tool_output = source.tool_output.clone();
    config.tool_output.max_bytes = config.tool_output.max_bytes.clamp(1, 64 * 1024);
    config.tool_output.max_lines = config.tool_output.max_lines.clamp(1, 1000);
    config.tool_output.max_results = config.tool_output.max_results.clamp(1, 256);
    config.inspection = source.inspection.clone();
    config.inspection.default_max_depth = config.inspection.default_max_depth.clamp(1, 16);
    config.mcp.enabled = false;
    config
}

async fn canonical_session(
    store: &StoreBundle,
    project_id: ProjectId,
    root_session_id: SessionId,
    workspace_root: &Utf8Path,
) -> Result<SessionRecord, PublishCallError> {
    let session = store
        .session_repo()
        .get_session(root_session_id)
        .await
        .map_err(|_| PublishCallError::InvalidTarget)?;
    if session.project_id != project_id
        || store
            .session_repo()
            .session_spawn_edge_for_child(session.id)
            .await
            .map_err(|_| PublishCallError::InvalidTarget)?
            .is_some()
        || store
            .side_chat_repo()
            .get_by_conversation(session.id)
            .map_err(|_| PublishCallError::InvalidTarget)?
            .is_some()
        || !PathGuard::same_existing_object_identity(&session.cwd, workspace_root)
            .map_err(|_| PublishCallError::InvalidTarget)?
    {
        return Err(PublishCallError::InvalidTarget);
    }
    Ok(session)
}

impl TargetSnapshot {
    pub(crate) fn workspace(&self) -> Option<&Workspace> {
        match self {
            Self::Temp => None,
            Self::Workspace(snapshot) => Some(&snapshot.workspace),
        }
    }

    pub(crate) async fn capture(
        store: &StoreBundle,
        target: &PublishTarget,
        config: &ResolvedConfig,
    ) -> Result<Self, PublishCallError> {
        let (project_id, workspace_root, legacy_session) = match target {
            PublishTarget::Temp {} => return Ok(Self::Temp),
            PublishTarget::Project {
                project_id,
                workspace_root,
            } => (*project_id, workspace_root, None),
            PublishTarget::LegacySession {
                project_id,
                root_session_id,
                workspace_root,
            } => {
                let session =
                    canonical_session(store, *project_id, *root_session_id, workspace_root).await?;
                (*project_id, workspace_root, Some(session))
            }
        };
        if !workspace_root.is_absolute()
            || workspace_root
                .components()
                .any(|part| matches!(part, Utf8Component::ParentDir))
        {
            return Err(PublishCallError::InvalidTarget);
        }
        let project = store
            .project_repo()
            .get_project(project_id)
            .await
            .map_err(|_| PublishCallError::InvalidTarget)?;
        // New project targets expose exactly the registered root. Legacy profiles keep
        // their narrower stored session workspace until the user explicitly replaces it.
        if legacy_session.is_none()
            && !PathGuard::same_existing_object_identity(workspace_root, &project.root_path)
                .map_err(|_| PublishCallError::InvalidTarget)?
        {
            return Err(PublishCallError::InvalidTarget);
        }
        let mut workspace = WorkspaceDiscovery::discover_with_stored_root(
            workspace_root,
            &project.root_path,
            config,
        )
        .map_err(|_| PublishCallError::InvalidTarget)?;
        workspace.project_id = project.id;
        let workspace_guard =
            PathGuard::require_path(&workspace, Utf8Path::new("."), AccessKind::List)
                .map_err(|_| PublishCallError::InvalidTarget)?;
        let project_guard = PathGuard::trusted_exact_path(&project.root_path)
            .map_err(|_| PublishCallError::InvalidTarget)?;
        Ok(Self::Workspace(WorkspaceTargetSnapshot {
            workspace,
            project_root: project.root_path,
            project_created_at_ms: project.created_at_ms,
            legacy_session_created_at_ms: legacy_session.map(|session| session.created_at_ms),
            workspace_pin: DirectoryPin::open(workspace_guard)?,
            project_pin: DirectoryPin::open(project_guard)?,
        }))
    }

    pub(crate) async fn verify(
        &self,
        store: &StoreBundle,
        target: &PublishTarget,
    ) -> Result<(), PublishCallError> {
        let Self::Workspace(snapshot) = self else {
            return if matches!(target, PublishTarget::Temp {}) {
                Ok(())
            } else {
                Err(PublishCallError::TargetChanged)
            };
        };
        let (project_id, workspace_root) = match target {
            PublishTarget::Temp {} => return Err(PublishCallError::TargetChanged),
            PublishTarget::Project {
                project_id,
                workspace_root,
            } if snapshot.legacy_session_created_at_ms.is_none() => (*project_id, workspace_root),
            PublishTarget::LegacySession {
                project_id,
                root_session_id,
                workspace_root,
            } => {
                let session =
                    canonical_session(store, *project_id, *root_session_id, workspace_root)
                        .await
                        .map_err(|_| PublishCallError::TargetChanged)?;
                if Some(session.created_at_ms) != snapshot.legacy_session_created_at_ms
                    || session.cwd != snapshot.workspace.cwd
                {
                    return Err(PublishCallError::TargetChanged);
                }
                (*project_id, workspace_root)
            }
            _ => return Err(PublishCallError::TargetChanged),
        };
        let project = store
            .project_repo()
            .get_project(project_id)
            .await
            .map_err(|_| PublishCallError::TargetChanged)?;
        if project.root_path != snapshot.project_root
            || project.created_at_ms != snapshot.project_created_at_ms
            || workspace_root != &snapshot.workspace.cwd
        {
            return Err(PublishCallError::TargetChanged);
        }
        snapshot.workspace_pin.verify()?;
        snapshot.project_pin.verify()?;
        Ok(())
    }
}

/// Shared save/start validation. Reads canonical identities without acquiring a Main run
/// admission or loading user/assistant/tool history. Filesystem work stays off the UI task.
pub async fn validate_target(
    store: &StoreBundle,
    target: &PublishTarget,
    protected_roots: &[Utf8PathBuf],
) -> Result<(), PublishCallError> {
    if matches!(target, PublishTarget::Temp {}) {
        return Ok(());
    }
    let store = store.clone();
    let target = target.clone();
    let protected_roots = protected_roots.to_vec();
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| PublishCallError::Unavailable)?;
        runtime.block_on(async {
            let config = read_config(&ResolvedConfig::default(), &protected_roots, &store);
            let snapshot = TargetSnapshot::capture(&store, &target, &config).await?;
            snapshot.verify(&store, &target).await.map(|_| ())
        })
    })
    .await
    .map_err(|_| PublishCallError::Unavailable)?
}

impl PublishReadDispatcher {
    pub async fn new(
        profile: PublishProfile,
        store: StoreBundle,
        config: ResolvedConfig,
        protected_roots: Vec<Utf8PathBuf>,
    ) -> Result<Self, PublishCallError> {
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| PublishCallError::Unavailable)?;
            runtime.block_on(async {
                profile
                    .validate()
                    .map_err(|_| PublishCallError::Unavailable)?;
                if !profile.enabled
                    || profile.authentication == (PublishAuthentication::Unpaired {})
                    || !matches!(profile.mode, super::PublishMode::ReadTools {})
                {
                    return Err(PublishCallError::Unavailable);
                }
                let config = read_config(&config, &protected_roots, &store);
                let mut registry = ToolRegistry::core_agent();
                registry.retain_tools(|name| profile.tools.contains(&ToolName::parse(name)));
                profile
                    .preview_tool_specs(&registry)
                    .map_err(|_| PublishCallError::ToolUnavailable)?;
                let target = TargetSnapshot::capture(&store, &profile.target, &config).await?;
                target.verify(&store, &profile.target).await?;
                Ok(Self {
                    inner: Arc::new(ReadDispatcherInner {
                        profile,
                        store,
                        config,
                        registry,
                        target,
                    }),
                })
            })
        })
        .await
        .map_err(|_| PublishCallError::Unavailable)?
    }
}

impl ReadDispatcherInner {
    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
        control: RunControl,
    ) -> Result<Value, PublishCallError> {
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        let spec = self
            .profile
            .preview_tool_specs(&self.registry)
            .map_err(|_| PublishCallError::ToolUnavailable)?
            .into_iter()
            .find(|spec| spec.name == ToolName::parse(name))
            .ok_or(PublishCallError::ToolUnavailable)?;
        if serde_json::to_vec(&arguments)
            .map_err(|_| PublishCallError::InvalidArguments)?
            .len()
            > MAX_ARGUMENT_BYTES
        {
            return Err(PublishCallError::InvalidArguments);
        }
        let arguments = if arguments.is_null() && spec.name == ToolName::CurrentTime {
            json!({})
        } else {
            arguments
        };
        let object = arguments
            .as_object()
            .ok_or(PublishCallError::InvalidArguments)?;
        let fields = spec.input_schema["properties"]
            .as_object()
            .ok_or(PublishCallError::Unavailable)?;
        if object.keys().any(|field| !fields.contains_key(field)) {
            return Err(PublishCallError::InvalidArguments);
        }
        self.target
            .verify(&self.store, &self.profile.target)
            .await?;
        if spec.name != ToolName::CurrentTime {
            let path = match object.get("path") {
                None if spec.name != ToolName::Read => ".",
                Some(Value::String(path))
                    if !path.is_empty() && path.len() <= 4096 && !path.contains('\0') =>
                {
                    path
                }
                _ => return Err(PublishCallError::InvalidArguments),
            };
            // Reject internal-output fallback and every additional/external root before the
            // tool sees its arguments; its existing PathGuard still rechecks the actual open.
            PathGuard::require_path(
                self.target
                    .workspace()
                    .ok_or(PublishCallError::ToolUnavailable)?,
                Utf8Path::new(path),
                AccessKind::Read,
            )
            .map_err(|_| PublishCallError::InvalidArguments)?;
        }
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        let result = if spec.name == ToolName::CurrentTime {
            crate::tool::current_time::execute_current_time(arguments)
        } else {
            self.registry
                .execute_published_read(
                    name,
                    arguments,
                    ReadToolContext::published(
                        self.target
                            .workspace()
                            .ok_or(PublishCallError::ToolUnavailable)?,
                        &self.config,
                        control,
                    ),
                )
                .await
        };
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        self.target
            .verify(&self.store, &self.profile.target)
            .await?;
        match result {
            Ok(result) => {
                if !result.recorded_changes.is_empty()
                    || !result.change_summaries.is_empty()
                    || result.truncated_output_path.is_some()
                {
                    return Err(PublishCallError::Unavailable);
                }
                let output = json!({"content": [{"type": "text", "text": result.output_text}], "structuredContent": result.metadata, "isError": false});
                if serde_json::to_vec(&output)
                    .map_err(|_| PublishCallError::Unavailable)?
                    .len()
                    > MAX_RESULT_BYTES
                {
                    return Ok(
                        json!({"content": [{"type": "text", "text": "The tool result exceeded the published output limit."}], "isError": true}),
                    );
                }
                Ok(output)
            }
            Err(_) => Ok(
                json!({"content": [{"type": "text", "text": "The published read could not complete. Check the arguments and allowed workspace."}], "isError": true}),
            ),
        }
    }
}

#[async_trait]
impl PublishToolDispatcher for PublishReadDispatcher {
    fn tool_descriptors(&self) -> Vec<Value> {
        self.profile_specs()
    }

    async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value, PublishCallError> {
        if cancel.is_cancelled() {
            return Err(PublishCallError::Cancelled);
        }
        let inner = Arc::clone(&self.inner);
        let name = name.to_string();
        let control = RunControl::new();
        let cancellation_control = control.clone();
        let operation_cancel = cancel.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| PublishCallError::Unavailable)?;
            runtime.block_on(inner.execute(&name, arguments, operation_cancel, control))
        });
        tokio::select! {
            result = &mut worker => result.map_err(|_| PublishCallError::Unavailable)?,
            _ = cancel.cancelled() => {
                cancellation_control.interrupt(crate::protocol::TurnInterruptionCause::UserStop);
                // Synchronous guarded reads cannot be forcibly interrupted. Keep this
                // operation owned until the worker settles; never return its cancelled data.
                let _ = worker.await;
                Err(PublishCallError::Cancelled)
            }
        }
    }
}

impl PublishReadDispatcher {
    fn profile_specs(&self) -> Vec<Value> {
        self.inner.profile.preview_tool_specs(&self.inner.registry).unwrap_or_default().into_iter().map(|spec| {
            let description = if spec.name == ToolName::Read {
                "Read bounded text in the published workspace without recording an edit baseline or granting edit authority."
            } else { spec.description };
            json!({"name": spec.name.to_string(), "description": description, "inputSchema": spec.input_schema,
                "annotations": {"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false}})
        }).collect()
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
pub(crate) mod tests;
