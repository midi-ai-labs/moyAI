pub mod discovery;
pub mod ignore;
pub mod path_guard;
pub mod project;
pub mod review;
pub mod special_paths;

pub use discovery::{Workspace, WorkspaceDiscovery};
pub use ignore::IgnorePlan;
pub use path_guard::{AccessKind, GuardedPath, PathGuard, PathPolicy};
pub use project::VcsKind;
pub use review::{branch_review_scope, uncommitted_review_scope};
pub use special_paths::{
    instruction_file_names, is_instruction_file, is_protected_workspace_authority_path,
    is_rule_file, is_skill_file, skill_roots,
};
