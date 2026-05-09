use camino::Utf8Path;
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::WorkspaceError;

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
            let glob = Glob::new(pattern).map_err(|error| {
                WorkspaceError::Message(format!("invalid ignore glob `{pattern}`: {error}"))
            })?;
            builder.add(glob);
        }
        builder.build().map_err(|error| {
            WorkspaceError::Message(format!("failed to compile ignore globs: {error}"))
        })
    }

    pub fn matches_compiled(&self, compiled: &GlobSet, root: &Utf8Path, path: &Utf8Path) -> bool {
        let relative = path.strip_prefix(root).unwrap_or(path);
        compiled.is_match(relative.as_str()) || compiled.is_match(path.as_str())
    }
}
