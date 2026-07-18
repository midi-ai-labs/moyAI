use camino::{Utf8Path, Utf8PathBuf};

use crate::workspace::path_guard::PathGuard;

const INSTRUCTION_FILE_NAMES: [&str; 4] = ["AGENTS.md", "AGENT.md", "AGENTS.local.md", "CLAUDE.md"];
const SKILL_ROOTS: [(&str, &str); 3] = [
    (".moyai", "skills"),
    (".agents", "skills"),
    (".claude", "skills"),
];

pub fn instruction_file_names() -> &'static [&'static str] {
    &INSTRUCTION_FILE_NAMES
}

pub fn skill_roots(root: &Utf8Path) -> Vec<Utf8PathBuf> {
    SKILL_ROOTS
        .into_iter()
        .map(|(prefix, skill_dir)| root.join(prefix).join(skill_dir))
        .collect()
}

pub fn is_instruction_file(path: &Utf8Path) -> bool {
    let Some(file_name) = path.file_name() else {
        return false;
    };
    INSTRUCTION_FILE_NAMES
        .iter()
        .any(|candidate| file_name.eq_ignore_ascii_case(candidate))
}

pub fn is_rule_file(root: &Utf8Path, path: &Utf8Path) -> bool {
    relative_workspace_path(root, path).is_some_and(|relative| {
        let mut components = relative.components();
        let Some(first) = components.next() else {
            return false;
        };
        let Some(second) = components.next() else {
            return false;
        };
        authority_component_eq(first.as_str(), ".moyai")
            && (authority_component_eq(second.as_str(), "rules")
                || authority_component_starts_with(second.as_str(), "rules-"))
    })
}

pub fn is_skill_file(root: &Utf8Path, path: &Utf8Path) -> bool {
    let Some(relative) = relative_workspace_path(root, path) else {
        return false;
    };
    if !relative
        .file_name()
        .is_some_and(|file_name| file_name.eq_ignore_ascii_case("SKILL.md"))
    {
        return false;
    }

    let mut components = relative.components();
    let Some(first) = components.next() else {
        return false;
    };
    let Some(second) = components.next() else {
        return false;
    };
    SKILL_ROOTS.iter().any(|(prefix, skill_dir)| {
        authority_component_eq(first.as_str(), prefix)
            && authority_component_eq(second.as_str(), skill_dir)
    })
}

pub(super) fn is_protected_workspace_authority_path(root: &Utf8Path, path: &Utf8Path) -> bool {
    is_instruction_file(path) || is_rule_file(root, path) || is_skill_file(root, path)
}

fn relative_workspace_path(root: &Utf8Path, path: &Utf8Path) -> Option<Utf8PathBuf> {
    PathGuard::relative_path_from_root(path, root)
}

fn authority_component_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn authority_component_starts_with(value: &str, prefix: &str) -> bool {
    if cfg!(windows) {
        value
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
    } else {
        value.starts_with(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_case_variants_preserve_workspace_authority_classification() {
        let root = Utf8Path::new("C:/Workspace");

        assert!(is_rule_file(
            root,
            Utf8Path::new("c:/workspace/.MOYAI/RuLeS-Team/policy.md")
        ));
        assert!(is_skill_file(
            root,
            Utf8Path::new("c:/workspace/.AGENTS/SKILLS/example/SKILL.md")
        ));
        assert!(is_protected_workspace_authority_path(
            root,
            Utf8Path::new("c:/workspace/.CLAUDE/SKILLS/example/skill.md")
        ));
        assert!(is_rule_file(
            root,
            Utf8Path::new(r"\\?\c:\WORKSPACE\.MOYAI\RULES-Team\policy.md")
        ));
        assert!(is_skill_file(
            Utf8Path::new(r"\\Server\Share\Workspace"),
            Utf8Path::new(r"\\?\uNc\SERVER\SHARE\workspace\.AGENTS\SKILLS\example\SKILL.md")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_case_variants_remain_distinct_workspace_paths() {
        let root = Utf8Path::new("/workspace");

        assert!(!is_rule_file(
            root,
            Utf8Path::new("/workspace/.MOYAI/RULES/policy.md")
        ));
        assert!(!is_skill_file(
            root,
            Utf8Path::new("/workspace/.AGENTS/SKILLS/example/SKILL.md")
        ));
        assert!(!is_rule_file(
            root,
            Utf8Path::new("/Workspace/.moyai/rules/policy.md")
        ));
    }
}
