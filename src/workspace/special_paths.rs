use camino::{Utf8Path, Utf8PathBuf};

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

pub fn is_workspace_config_path(root: &Utf8Path, path: &Utf8Path) -> bool {
    relative_workspace_path(root, path).is_some_and(|relative| {
        relative == Utf8Path::new("moyai.toml") || relative == Utf8Path::new(".moyai/config.toml")
    })
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
        first.as_str() == ".moyai"
            && (second.as_str() == "rules" || second.as_str().starts_with("rules-"))
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
    SKILL_ROOTS
        .iter()
        .any(|(prefix, skill_dir)| first.as_str() == *prefix && second.as_str() == *skill_dir)
}

pub fn is_protected_instruction_or_config_path(root: &Utf8Path, path: &Utf8Path) -> bool {
    is_instruction_file(path)
        || is_workspace_config_path(root, path)
        || is_rule_file(root, path)
        || is_skill_file(root, path)
}

fn relative_workspace_path<'a>(root: &'a Utf8Path, path: &'a Utf8Path) -> Option<&'a Utf8Path> {
    path.starts_with(root)
        .then_some(())
        .and_then(|_| path.strip_prefix(root).ok())
}
