use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;

use crate::workspace::skill_roots;

const SKILL_FILE_NAME: &str = "SKILL.md";
const MAX_SAMPLED_FILES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub description: String,
    pub path: Utf8PathBuf,
    pub base_dir: Utf8PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub manifest: DiscoveredSkill,
    pub content: String,
    pub sampled_files: Vec<Utf8PathBuf>,
}

pub fn discover(root: &Utf8Path) -> Vec<DiscoveredSkill> {
    let mut skills = Vec::new();
    for skill_root in skill_roots(root) {
        if !skill_root.exists() {
            continue;
        }
        let mut builder = WalkBuilder::new(&skill_root);
        builder.hidden(false);
        for entry in builder.build().flatten() {
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) else {
                continue;
            };
            if !path
                .file_name()
                .is_some_and(|file_name| file_name.eq_ignore_ascii_case(SKILL_FILE_NAME))
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(path.as_std_path()) else {
                continue;
            };
            let (name, description) = parse_skill_manifest(&path, &text);
            skills.push(DiscoveredSkill {
                name,
                description,
                base_dir: path.parent().unwrap_or(root).to_path_buf(),
                path,
            });
        }
    }
    skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    skills
}

pub fn load(root: &Utf8Path, name: &str) -> Result<Option<LoadedSkill>, String> {
    let manifest = discover(root)
        .into_iter()
        .find(|skill| skill.name.eq_ignore_ascii_case(name));
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let content = fs::read_to_string(manifest.path.as_std_path()).map_err(|error| {
        format!(
            "failed to read skill `{}` from {}: {error}",
            manifest.name, manifest.path
        )
    })?;
    let sampled_files = sample_related_files(&manifest.base_dir)?;
    Ok(Some(LoadedSkill {
        manifest,
        content,
        sampled_files,
    }))
}

pub fn render_available_skills(root: &Utf8Path) -> String {
    let skills = discover(root);
    if skills.is_empty() {
        return "No local skills are currently available.".to_string();
    }
    let mut lines = vec![
        "Use the `skill` tool when the current task clearly matches one of these local skills:"
            .to_string(),
    ];
    for skill in skills {
        lines.push(format!("- {}: {}", skill.name, skill.description));
    }
    lines.join("\n")
}

fn parse_skill_manifest(path: &Utf8Path, text: &str) -> (String, String) {
    let mut frontmatter_name = None;
    let mut frontmatter_description = None;
    if let Some(frontmatter) = extract_frontmatter(text) {
        for line in frontmatter.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            match key.trim() {
                "name" => frontmatter_name = parse_frontmatter_value(value),
                "description" => frontmatter_description = parse_frontmatter_value(value),
                _ => {}
            }
        }
    }

    let fallback_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(ToString::to_string)
        .unwrap_or_else(|| "skill".to_string());
    let heading_name = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim().to_string());
    let description = frontmatter_description
        .or_else(|| first_body_line(text))
        .unwrap_or_else(|| "No description.".to_string());
    let name = frontmatter_name
        .or(heading_name)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name);
    (name, description)
}

fn extract_frontmatter(text: &str) -> Option<&str> {
    let stripped = text.strip_prefix("---\n")?;
    let end = stripped.find("\n---")?;
    Some(&stripped[..end])
}

fn parse_frontmatter_value(value: &str) -> Option<String> {
    let parsed = value.trim().trim_matches('"').trim_matches('\'');
    (!parsed.is_empty()).then(|| parsed.to_string())
}

fn first_body_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && *line != "---")
        .map(ToString::to_string)
}

fn sample_related_files(base_dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, String> {
    let mut files = Vec::new();
    let mut builder = WalkBuilder::new(base_dir);
    builder.hidden(false);
    for entry in builder.build().flatten() {
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let path = Utf8PathBuf::from_path_buf(entry.into_path())
            .map_err(|_| "skill file path is not valid UTF-8".to_string())?;
        if path
            .file_name()
            .is_some_and(|file_name| file_name.eq_ignore_ascii_case(SKILL_FILE_NAME))
        {
            continue;
        }
        files.push(path);
        if files.len() >= MAX_SAMPLED_FILES {
            break;
        }
    }
    Ok(files)
}
