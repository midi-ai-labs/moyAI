use std::collections::BTreeMap;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::config::PermissionProfileCatalog;
use crate::config::{AccessMode, ResolvedConfig};
use crate::context::current_time::CurrentTimeSnapshot;
use crate::workspace::{Workspace, instruction_file_names};

const MAX_CONTEXT_SOURCE_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_TOTAL_BYTES: usize = 48 * 1024;

pub trait WorldStateSection {
    fn section_id(&self) -> &'static str;
    fn snapshot_json(&self) -> serde_json::Value;
    fn render(&self) -> String;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldStateSnapshot {
    pub sections: BTreeMap<String, serde_json::Value>,
}

impl WorldStateSnapshot {
    pub fn from_sections(sections: &[&dyn WorldStateSection]) -> Self {
        let sections = sections
            .iter()
            .map(|section| (section.section_id().to_string(), section.snapshot_json()))
            .collect();
        Self { sections }
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    pub snapshot: WorldStateSnapshot,
    pub rendered: String,
}

impl WorldState {
    pub fn build(workspace: &Workspace, config: &ResolvedConfig, tools: &[String]) -> Self {
        let environment = EnvironmentSection::new(workspace, config, tools);
        let instructions = InstructionsSection::load(workspace, config);
        let time = CurrentTimeSection {
            snapshot: CurrentTimeSnapshot::now(),
        };
        let sections: Vec<&dyn WorldStateSection> = vec![&environment, &instructions, &time];
        let snapshot = WorldStateSnapshot::from_sections(&sections);
        let rendered = render_world_state(&sections);
        Self { snapshot, rendered }
    }
}

fn render_world_state(sections: &[&dyn WorldStateSection]) -> String {
    let body = sections
        .iter()
        .map(|section| section.render())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("<world_state>\n{body}\n</world_state>")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSection {
    pub workspace_root: Utf8PathBuf,
    pub cwd: Utf8PathBuf,
    pub access_mode: AccessMode,
    pub model: String,
    pub base_url: String,
    pub shell_family: String,
    pub tools: Vec<String>,
    pub permission_profile_summary: String,
}

impl EnvironmentSection {
    fn new(workspace: &Workspace, config: &ResolvedConfig, tools: &[String]) -> Self {
        Self {
            workspace_root: workspace.root.clone(),
            cwd: workspace.cwd.clone(),
            access_mode: config.permissions.access_mode,
            model: config.model.model.clone(),
            base_url: config.model.base_url.clone(),
            shell_family: config
                .shell
                .family
                .map(|family| format!("{family:?}"))
                .unwrap_or_else(|| "auto".to_string()),
            tools: tools.to_vec(),
            permission_profile_summary: PermissionProfileCatalog::for_current(
                config.permissions.access_mode,
            )
            .selected_profile()
            .map(|profile| profile.summary.clone())
            .unwrap_or_else(|| "Unknown permission profile.".to_string()),
        }
    }
}

impl WorldStateSection for EnvironmentSection {
    fn section_id(&self) -> &'static str {
        "environment"
    }

    fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn render(&self) -> String {
        format!(
            "<environment_context>\n<workspace_root>{}</workspace_root>\n<cwd>{}</cwd>\n<access_mode>{}</access_mode>\n<permission_profile>{}</permission_profile>\n<model>{}</model>\n<base_url>{}</base_url>\n<shell>{}</shell>\n<tools>{}</tools>\n</environment_context>",
            self.workspace_root,
            self.cwd,
            self.access_mode.as_str(),
            self.permission_profile_summary,
            self.model,
            self.base_url,
            self.shell_family,
            if self.tools.is_empty() {
                "none".to_string()
            } else {
                self.tools.join(", ")
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSource {
    pub path: Utf8PathBuf,
    pub relative_path: String,
    pub kind: InstructionKind,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionKind {
    Agents,
    Rules,
    Configured,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionsSection {
    pub sources: Vec<InstructionSource>,
    pub truncated: bool,
}

impl InstructionsSection {
    pub fn load(workspace: &Workspace, config: &ResolvedConfig) -> Self {
        let mut candidates = instruction_candidates(workspace, config);
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        candidates.dedup_by(|left, right| left.0 == right.0);

        let mut total_bytes = 0usize;
        let mut sources = Vec::new();
        let mut truncated = false;
        for (path, kind) in candidates {
            if !path.exists() {
                continue;
            }
            let Ok(raw) = fs::read_to_string(path.as_std_path()) else {
                continue;
            };
            if raw.trim().is_empty() {
                continue;
            }
            let remaining = MAX_CONTEXT_TOTAL_BYTES.saturating_sub(total_bytes);
            if remaining == 0 {
                truncated = true;
                break;
            }
            let limit = remaining.min(MAX_CONTEXT_SOURCE_BYTES);
            let (content, source_truncated) = clip_to_char_boundary(&raw, limit);
            total_bytes += content.len();
            truncated |= source_truncated;
            sources.push(InstructionSource {
                relative_path: relative_display_path(&workspace.root, &path),
                path,
                kind,
                content,
                truncated: source_truncated,
            });
        }
        Self { sources, truncated }
    }
}

impl WorldStateSection for InstructionsSection {
    fn section_id(&self) -> &'static str {
        "instructions"
    }

    fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn render(&self) -> String {
        if self.sources.is_empty() {
            return "<instructions>No workspace instruction files are currently visible.</instructions>".to_string();
        }
        let mut out = String::from(
            "<instructions>\nThese workspace instructions replace previously provided workspace instructions for this turn.\n",
        );
        for source in &self.sources {
            out.push_str(&format!(
                "\n<instruction source=\"{}\" kind=\"{:?}\" truncated=\"{}\">\n{}\n</instruction>",
                source.relative_path,
                source.kind,
                source.truncated,
                source.content.trim()
            ));
        }
        if self.truncated {
            out.push_str("\n<instruction_truncation>Some instruction content was truncated to keep the prompt bounded.</instruction_truncation>");
        }
        out.push_str("\n</instructions>");
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurrentTimeSection {
    pub snapshot: CurrentTimeSnapshot,
}

impl WorldStateSection for CurrentTimeSection {
    fn section_id(&self) -> &'static str {
        "current_time"
    }

    fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn render(&self) -> String {
        format!(
            "<current_time local=\"{}\" utc=\"{}\" timezone=\"{}\" />",
            self.snapshot.local, self.snapshot.utc, self.snapshot.timezone
        )
    }
}

fn instruction_candidates(
    workspace: &Workspace,
    config: &ResolvedConfig,
) -> Vec<(Utf8PathBuf, InstructionKind)> {
    let mut candidates = Vec::new();
    let mut current = Some(workspace.cwd.as_path());
    while let Some(dir) = current {
        for file_name in instruction_file_names() {
            candidates.push((dir.join(file_name), InstructionKind::Agents));
        }
        if dir == workspace.root {
            break;
        }
        current = dir.parent();
    }
    candidates.extend(rule_candidates(&workspace.root));
    candidates.extend(config.instructions.additional_files.iter().map(|path| {
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            workspace.root.join(path)
        };
        (resolved, InstructionKind::Configured)
    }));
    candidates
}

fn rule_candidates(root: &Utf8Path) -> Vec<(Utf8PathBuf, InstructionKind)> {
    let moyai_dir = root.join(".moyai");
    if !moyai_dir.exists() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for entry in WalkBuilder::new(&moyai_dir).hidden(false).build().flatten() {
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) else {
            continue;
        };
        let Some(relative) = path.strip_prefix(root).ok() else {
            continue;
        };
        let mut components = relative.components();
        let Some(first) = components.next() else {
            continue;
        };
        let Some(second) = components.next() else {
            continue;
        };
        if first.as_str() == ".moyai"
            && (second.as_str() == "rules" || second.as_str().starts_with("rules-"))
        {
            candidates.push((path, InstructionKind::Rules));
        }
    }
    candidates
}

fn relative_display_path(root: &Utf8Path, path: &Utf8Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.as_str().replace('\\', "/"))
        .unwrap_or_else(|_| path.as_str().replace('\\', "/"))
}

fn clip_to_char_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut clipped = text[..end].to_string();
    clipped.push_str("\n[truncated]");
    (clipped, true)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use crate::config::ResolvedConfig;
    use crate::session::ProjectId;
    use crate::workspace::{IgnorePlan, PathPolicy, VcsKind, Workspace};

    fn workspace(root: Utf8PathBuf) -> Workspace {
        Workspace {
            project_id: ProjectId::from_stable_input(root.as_str()),
            cwd: root.clone(),
            path_policy: PathPolicy {
                workspace_root: root.clone(),
                additional_read_roots: Vec::new(),
                additional_write_roots: Vec::new(),
            },
            root,
            vcs: VcsKind::None,
            ignore: IgnorePlan::default_with(Vec::new()),
            protected_paths: Vec::new(),
        }
    }

    #[test]
    fn world_state_loads_agents_and_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8");
        std::fs::write(root.join("AGENTS.md"), "Follow workspace rules.").expect("agents");
        std::fs::create_dir_all(root.join(".moyai/rules")).expect("rules dir");
        std::fs::write(root.join(".moyai/rules/style.md"), "Use compact edits.").expect("rule");

        let ws = workspace(root);
        let state =
            super::WorldState::build(&ws, &ResolvedConfig::default(), &["read".to_string()]);

        assert!(state.rendered.contains("Follow workspace rules."));
        assert!(state.rendered.contains("Use compact edits."));
        assert!(state.snapshot.sections.contains_key("environment"));
        assert!(state.snapshot.sections.contains_key("instructions"));
        assert!(state.snapshot.sections.contains_key("current_time"));
    }
}
