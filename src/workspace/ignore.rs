use camino::Utf8Path;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::WorkspaceError;
use crate::workspace::path_guard::PathGuard;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IgnorePlan {
    pub builtin_patterns: Vec<String>,
    pub custom_patterns: Vec<String>,
    pub use_gitignore: bool,
}

impl IgnorePlan {
    pub fn default_with(custom_patterns: Vec<String>) -> Self {
        Self {
            builtin_patterns: vec![
                ".git/**".to_string(),
                "**/.git/**".to_string(),
                "node_modules/**".to_string(),
                "**/node_modules/**".to_string(),
                "target/**".to_string(),
                "**/target/**".to_string(),
                "dist/**".to_string(),
                "**/dist/**".to_string(),
                "build/**".to_string(),
                "**/build/**".to_string(),
                ".venv/**".to_string(),
                "**/.venv/**".to_string(),
                "coverage/**".to_string(),
                "**/coverage/**".to_string(),
                "tmp/**".to_string(),
                "**/tmp/**".to_string(),
            ],
            custom_patterns,
            use_gitignore: true,
        }
    }

    pub fn compile(&self) -> Result<GlobSet, WorkspaceError> {
        let mut builder = GlobSetBuilder::new();
        for pattern in self
            .builtin_patterns
            .iter()
            .chain(self.custom_patterns.iter())
        {
            let mut glob_builder = GlobBuilder::new(pattern);
            glob_builder.case_insensitive(cfg!(windows));
            let glob = glob_builder.build().map_err(|error| {
                WorkspaceError::Message(format!("invalid ignore glob `{pattern}`: {error}"))
            })?;
            builder.add(glob);
        }
        builder.build().map_err(|error| {
            WorkspaceError::Message(format!("failed to compile ignore globs: {error}"))
        })
    }

    pub fn matches_compiled(&self, compiled: &GlobSet, root: &Utf8Path, path: &Utf8Path) -> bool {
        PathGuard::relative_path_from_root(path, root)
            .is_some_and(|relative| compiled.is_match(relative.as_str()))
            || compiled.is_match(path.as_str())
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::IgnorePlan;

    #[cfg(windows)]
    #[test]
    fn windows_ignore_globs_match_case_and_extended_root_aliases() {
        let plan = IgnorePlan::default_with(vec!["Generated/**".to_string()]);
        let compiled = plan.compile().expect("ignore globs");

        assert!(plan.matches_compiled(
            &compiled,
            Utf8Path::new(r"C:\Workspace"),
            Utf8Path::new(r"\\?\c:\WORKSPACE\generated\deep\file.rs"),
        ));
        assert!(plan.matches_compiled(
            &compiled,
            Utf8Path::new(r"\\Server\Share\Workspace"),
            Utf8Path::new(r"\\?\uNc\SERVER\SHARE\workspace\GENERATED\child"),
        ));
        assert!(plan.matches_compiled(
            &compiled,
            Utf8Path::new(r"C:\Workspace"),
            Utf8Path::new(r"c:\workspace\TARGET\debug\artifact"),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_ignore_globs_and_root_projection_remain_case_sensitive() {
        let plan = IgnorePlan::default_with(vec!["Generated/**".to_string()]);
        let compiled = plan.compile().expect("ignore globs");

        assert!(plan.matches_compiled(
            &compiled,
            Utf8Path::new("/workspace"),
            Utf8Path::new("/workspace/Generated/deep/file.rs"),
        ));
        assert!(!plan.matches_compiled(
            &compiled,
            Utf8Path::new("/workspace"),
            Utf8Path::new("/workspace/generated/deep/file.rs"),
        ));
        assert!(!plan.matches_compiled(
            &compiled,
            Utf8Path::new("/Workspace"),
            Utf8Path::new("/workspace/Generated/deep/file.rs"),
        ));
    }
}
