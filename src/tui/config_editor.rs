use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::NamedTempFile;

use crate::config::field::{build_resolved_config_from_field_values, parse_config_field_patch};
use crate::config::loader::{
    acquire_global_config_write_lease, global_config_path, read_toml_utf8_bounded,
};
use crate::config::model::{AccessMode, ResolvedConfig};
use crate::config::{ConfigField, ConfigLoader, ProviderEndpoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalConfigAdoptionPolicy {
    StrictCurrentSchema,
    PreserveUnknownTopLevelSections,
}

#[derive(Debug, Clone)]
pub struct GlobalConfigSaveResult {
    pub message: String,
    pub resolved_config: ResolvedConfig,
    pub preserved_unknown_top_level_sections: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ConfigFieldState {
    pub key: ConfigField,
    pub value: String,
    pub dirty: bool,
}

#[derive(Debug, Clone)]
pub struct ConfigEditorState {
    pub fields: Vec<ConfigFieldState>,
    pub selected: usize,
    pub feedback: Option<String>,
}

impl ConfigEditorState {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        Self {
            fields: ConfigField::ALL
                .into_iter()
                .map(|key| ConfigFieldState {
                    key,
                    value: key.value(config),
                    dirty: false,
                })
                .collect(),
            selected: 0,
            feedback: None,
        }
    }

    pub fn from_config_values(
        config: &ResolvedConfig,
        values: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let mut candidate = Self::from_config(config);
        candidate.replace_values_by_key(values)?;
        Ok(candidate)
    }

    pub fn replace_values_by_key(&mut self, values: Vec<(String, String)>) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut updates = Vec::with_capacity(values.len());
        for (key, value) in values {
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate config field key: {key}"));
            }
            let index = self
                .fields
                .iter()
                .position(|field| field.key.label() == key)
                .ok_or_else(|| format!("unknown config field key: {key}"))?;
            updates.push((index, value));
        }
        for (index, value) in updates {
            let field = &mut self.fields[index];
            field.dirty = field.value != value;
            field.value = value;
        }
        Ok(())
    }

    pub fn selected_field(&self) -> &ConfigFieldState {
        &self.fields[self.selected]
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.fields.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1);
        self.selected = next as usize;
    }

    pub fn insert_char(&mut self, value: char) {
        self.fields[self.selected].value.push(value);
        self.fields[self.selected].dirty = true;
    }

    pub fn backspace(&mut self) {
        self.fields[self.selected].value.pop();
        self.fields[self.selected].dirty = true;
    }

    pub fn clear_selected(&mut self) {
        self.fields[self.selected].value.clear();
        self.fields[self.selected].dirty = true;
    }

    pub fn build_resolved_config(&self, base: &ResolvedConfig) -> Result<ResolvedConfig, String> {
        let fields = self
            .fields
            .iter()
            .map(|field| (field.key, field.value.as_str()))
            .collect::<Vec<_>>();
        build_resolved_config_from_field_values(base, &fields)
    }

    pub fn save_global(
        &self,
        _root: &Utf8Path,
        adoption_policy: GlobalConfigAdoptionPolicy,
    ) -> Result<GlobalConfigSaveResult, String> {
        let path = global_config_path().map_err(|error| error.to_string())?;
        let (resolved_config, preserved_unknown_top_level_sections) =
            save_config_sections(&path, self, adoption_policy)?;
        let mut message = format!("saved global config to {}", path);
        if !preserved_unknown_top_level_sections.is_empty() {
            let count = preserved_unknown_top_level_sections.len();
            message.push_str(&format!(
                "; preserved {count} unrecognized top-level config section{} without applying {}",
                if count == 1 { "" } else { "s" },
                if count == 1 { "it" } else { "them" },
            ));
        }
        Ok(GlobalConfigSaveResult {
            message,
            resolved_config,
            preserved_unknown_top_level_sections,
        })
    }

    pub fn remember_global_access_mode(access_mode: AccessMode) -> Result<Utf8PathBuf, String> {
        let path = global_config_path().map_err(|error| error.to_string())?;
        save_access_mode(&path, access_mode)?;
        Ok(path)
    }

    pub fn compare_and_set_global_access_mode(
        expected: AccessMode,
        access_mode: AccessMode,
    ) -> Result<Option<Utf8PathBuf>, String> {
        let path = global_config_path().map_err(|error| error.to_string())?;
        compare_and_set_access_mode(&path, expected, access_mode)
            .map(|updated| updated.then_some(path))
    }
}

fn save_access_mode(path: &Utf8Path, access_mode: AccessMode) -> Result<(), String> {
    write_access_mode(path, None, access_mode).map(|_| ())
}

fn compare_and_set_access_mode(
    path: &Utf8Path,
    expected: AccessMode,
    access_mode: AccessMode,
) -> Result<bool, String> {
    write_access_mode(path, Some(expected), access_mode)
}

fn write_access_mode(
    path: &Utf8Path,
    expected: Option<AccessMode>,
    access_mode: AccessMode,
) -> Result<bool, String> {
    let _write_lease =
        acquire_global_config_write_lease(path).map_err(|error| error.to_string())?;
    let mut existing = read_toml_document(path)?;
    let current = access_mode_from_document(&existing)?;
    if expected.is_some_and(|expected| current != expected) {
        return Ok(false);
    }
    let root = existing
        .as_table_mut()
        .ok_or_else(|| "global config root must be a TOML table".to_string())?;
    let permissions = root
        .entry("permissions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "global config section `permissions` must be a TOML table".to_string())?;
    permissions.insert(
        "access_mode".to_string(),
        toml::Value::String(access_mode.as_str().to_string()),
    );
    normalize_provider_endpoint_in_document(&mut existing)?;
    let text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    persist_config_tempfile(path, &text)?;
    Ok(true)
}

fn access_mode_from_document(document: &toml::Value) -> Result<AccessMode, String> {
    let Some(value) = document
        .get("permissions")
        .and_then(|permissions| permissions.get("access_mode"))
    else {
        return Ok(ResolvedConfig::default().permissions.access_mode);
    };
    let value = value
        .as_str()
        .ok_or_else(|| "permissions.access_mode must be a string".to_string())?;
    match value {
        "default" | "standard" => Ok(AccessMode::Default),
        "auto_review" | "auto-review" => Ok(AccessMode::AutoReview),
        "full_access" | "full-access" => Ok(AccessMode::FullAccess),
        _ => Err(format!("unknown permissions.access_mode `{value}`")),
    }
}

fn save_config_sections(
    path: &Utf8Path,
    editor: &ConfigEditorState,
    adoption_policy: GlobalConfigAdoptionPolicy,
) -> Result<(ResolvedConfig, Vec<String>), String> {
    let _write_lease =
        acquire_global_config_write_lease(path).map_err(|error| error.to_string())?;
    let (text, dirty) = prepare_config_section_update(path, editor)?;
    let (resolved_config, preserved_unknown_top_level_sections) = match adoption_policy {
        GlobalConfigAdoptionPolicy::StrictCurrentSchema => (
            ConfigLoader::resolve_global_config_text_with_environment(path, &text)
                .map_err(|error| error.to_string())?,
            Vec::new(),
        ),
        GlobalConfigAdoptionPolicy::PreserveUnknownTopLevelSections => {
            let resolved =
                ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
                    path, &text,
                )
                .map_err(|error| error.to_string())?;
            (
                resolved.resolved_config,
                resolved.preserved_unknown_top_level_sections,
            )
        }
    };
    if !dirty {
        return Ok((resolved_config, preserved_unknown_top_level_sections));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    persist_config_tempfile(path, &text)?;
    Ok((resolved_config, preserved_unknown_top_level_sections))
}

fn prepare_config_section_update(
    path: &Utf8Path,
    editor: &ConfigEditorState,
) -> Result<(String, bool), String> {
    let dirty_values = editor
        .fields
        .iter()
        .filter(|field| field.dirty)
        .map(|field| (field.key, field.value.as_str()))
        .collect::<Vec<_>>();
    let dirty = !dirty_values.is_empty();
    let mut existing = read_toml_document(path)?;
    if dirty {
        let patch = parse_config_field_patch(&dirty_values)?;
        let patch = toml::Value::try_from(patch).map_err(|error| error.to_string())?;
        for (field, _) in dirty_values {
            apply_dirty_toml_field(&mut existing, &patch, field)?;
        }
    }
    normalize_provider_endpoint_in_document(&mut existing)?;
    let text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
    Ok((text, dirty))
}

fn read_toml_document(path: &Utf8Path) -> Result<toml::Value, String> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let text = read_toml_utf8_bounded(path).map_err(|error| error.to_string())?;
    if text.trim().is_empty() {
        Ok(toml::Value::Table(toml::map::Map::new()))
    } else {
        toml::from_str(&text).map_err(|error| error.to_string())
    }
}

fn apply_dirty_toml_field(
    existing: &mut toml::Value,
    patch: &toml::Value,
    field: ConfigField,
) -> Result<(), String> {
    let (section_name, field_name) = field.toml_path();
    let patch_value = patch
        .get(section_name)
        .and_then(|section| section.get(field_name))
        .cloned();
    let root = existing
        .as_table_mut()
        .ok_or_else(|| "global config root must be a TOML table".to_string())?;

    if let Some(value) = patch_value {
        let section = root
            .entry(section_name.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let section = section.as_table_mut().ok_or_else(|| {
            format!("global config section `{section_name}` must be a TOML table")
        })?;
        section.insert(field_name.to_string(), value);
    } else if let Some(section) = root.get_mut(section_name) {
        let section = section.as_table_mut().ok_or_else(|| {
            format!("global config section `{section_name}` must be a TOML table")
        })?;
        section.remove(field_name);
    }
    if field == ConfigField::RequestTimeoutMs {
        if let Some(model) = root.get_mut("model") {
            let model = model
                .as_table_mut()
                .ok_or_else(|| "global config section `model` must be a TOML table".to_string())?;
            model.remove("stream_idle_timeout_ms");
        }
    }
    if field == ConfigField::ProviderProfile {
        if let Some(model) = root.get_mut("model") {
            let model = model
                .as_table_mut()
                .ok_or_else(|| "global config section `model` must be a TOML table".to_string())?;
            model.remove("provider_metadata_mode");
            model.remove("provider_api_mode");
        }
    }
    Ok(())
}

fn normalize_provider_endpoint_in_document(document: &mut toml::Value) -> Result<(), String> {
    let Some(model) = document.get_mut("model") else {
        return Ok(());
    };
    let model = model
        .as_table_mut()
        .ok_or_else(|| "global config section `model` must be a TOML table".to_string())?;
    let Some(base_url) = model.get_mut("base_url") else {
        return Ok(());
    };
    let raw = base_url
        .as_str()
        .ok_or_else(|| "model.base_url must be a string".to_string())?;
    let endpoint = ProviderEndpoint::parse(raw).map_err(|error| error.to_string())?;
    *base_url = toml::Value::String(endpoint.as_str().to_string());
    Ok(())
}

fn persist_config_tempfile(path: &Utf8Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path `{path}` has no parent directory"))?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(path.as_std_path())
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}

#[cfg(test)]
fn parse_editor_patch(
    editor: &ConfigEditorState,
) -> Result<crate::config::model::PartialResolvedConfig, String> {
    let fields = editor
        .fields
        .iter()
        .map(|field| (field.key, field.value.as_str()))
        .collect::<Vec<_>>();
    parse_config_field_patch(&fields)
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use camino::Utf8PathBuf;

    use super::{
        ConfigEditorState, ConfigField, GlobalConfigAdoptionPolicy, compare_and_set_access_mode,
        parse_editor_patch, save_access_mode,
    };
    use crate::config::{AccessMode, ProviderProfile, ResolvedConfig};

    fn save_config_sections(
        path: &camino::Utf8Path,
        editor: &ConfigEditorState,
    ) -> Result<ResolvedConfig, String> {
        super::save_config_sections(
            path,
            editor,
            GlobalConfigAdoptionPolicy::PreserveUnknownTopLevelSections,
        )
        .map(|(resolved_config, _)| resolved_config)
    }

    fn save_config_sections_resolved(
        path: &camino::Utf8Path,
        editor: &ConfigEditorState,
    ) -> Result<ResolvedConfig, String> {
        super::save_config_sections(
            path,
            editor,
            GlobalConfigAdoptionPolicy::StrictCurrentSchema,
        )
        .map(|(resolved_config, _)| resolved_config)
    }

    #[test]
    fn config_editor_excludes_removed_model_behavior_guards() {
        let editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let labels = editor
            .fields
            .iter()
            .map(|field| field.key.label())
            .collect::<Vec<_>>();

        assert!(!labels.contains(&"model.prompt_profile"));
        assert!(!labels.contains(&"session.max_steps_per_turn"));
    }

    #[test]
    fn config_editor_exposes_one_typed_llm_response_timeout() {
        let config = ResolvedConfig::default();
        let editor = ConfigEditorState::from_config(&config);
        let timeout_fields = editor
            .fields
            .iter()
            .filter(|field| field.key.label().contains("timeout"))
            .collect::<Vec<_>>();

        let response_timeout = editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::RequestTimeoutMs)
            .expect("canonical LLM response timeout field");

        assert_eq!(response_timeout.value, "3600000");
        assert_eq!(response_timeout.key.display_label(), "LLM response timeout");
        assert!(response_timeout.key.help().contains("stream完了"));
        assert!(
            editor
                .fields
                .iter()
                .all(|field| field.key.label() != "model.stream_idle_timeout_ms")
        );
        assert_eq!(
            timeout_fields
                .iter()
                .filter(|field| field.key.label().starts_with("model."))
                .count(),
            2,
            "the model surface retains the response and connect timeout settings only"
        );
    }

    #[test]
    fn config_value_candidate_uses_stable_keys_and_rejects_invalid_batch_atomically() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let original_model = editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field")
            .value
            .clone();

        let error = editor
            .replace_values_by_key(vec![
                ("model.model".to_string(), "changed-model".to_string()),
                ("unknown.field".to_string(), "invalid".to_string()),
            ])
            .expect_err("unknown field must reject the full batch");
        assert!(error.contains("unknown config field key"));
        let model = editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        assert_eq!(model.value, original_model);
        assert!(!model.dirty);

        let candidate = ConfigEditorState::from_config_values(
            &config,
            vec![("model.model".to_string(), "changed-model".to_string())],
        )
        .expect("known stable key");
        let model = candidate
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        assert_eq!(model.value, "changed-model");
        assert!(model.dirty);
    }

    #[test]
    fn complete_session_candidate_preserves_explicit_optional_absence() {
        let mut base = ResolvedConfig::default();
        base.model.temperature = Some(0.7);
        base.model.extra_body_json = Some(serde_json::json!({"num_ctx": 32768}));
        let candidate = ConfigEditorState::from_config_values(
            &base,
            vec![
                (ConfigField::Temperature.label().to_string(), String::new()),
                (
                    ConfigField::ExtraBodyJson.label().to_string(),
                    String::new(),
                ),
            ],
        )
        .expect("complete config values");

        let resolved = candidate
            .build_resolved_config(&base)
            .expect("complete config candidate");

        assert_eq!(resolved.model.temperature, None);
        assert_eq!(resolved.model.extra_body_json, None);
        assert_eq!(resolved.model.model, base.model.model);
    }

    #[test]
    fn complete_session_candidate_rejects_missing_required_values() {
        let base = ResolvedConfig::default();
        let candidate = ConfigEditorState::from_config_values(
            &base,
            vec![(ConfigField::Model.label().to_string(), String::new())],
        )
        .expect("complete config values");

        let error = candidate
            .build_resolved_config(&base)
            .expect_err("required model cannot be cleared");

        assert_eq!(error, "model.model must not be empty");
    }

    #[test]
    fn config_editor_projects_atomic_provider_profile_patch() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ProviderProfile)
            .expect("provider profile field is present");
        field.value = "openai_compatible".to_string();

        let patch = parse_editor_patch(&editor).expect("provider profile parses");

        assert_eq!(
            patch.model.and_then(|model| model.provider_profile),
            Some(ProviderProfile::OpenAiCompatible)
        );
    }

    #[test]
    fn saving_provider_profile_replaces_both_legacy_split_fields() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\nprovider_metadata_mode = \"lm_studio_native_required\"\nprovider_api_mode = \"responses\"\nmodel = \"keep-model\"\n",
        )
        .expect("legacy provider config");
        let mut effective = ResolvedConfig::default();
        effective.model.model = "keep-model".to_string();
        let mut editor = ConfigEditorState::from_config(&effective);
        let profile = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ProviderProfile)
            .expect("provider profile field");
        profile.value = "openai_compatible".to_string();
        profile.dirty = true;

        let resolved = save_config_sections(&path, &editor).expect("save canonical profile");
        assert_eq!(
            resolved.model.provider_profile,
            ProviderProfile::OpenAiCompatible
        );
        let saved = std::fs::read_to_string(&path).expect("saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["model"]["provider_profile"].as_str(),
            Some("openai_compatible")
        );
        assert!(saved["model"].get("provider_metadata_mode").is_none());
        assert!(saved["model"].get("provider_api_mode").is_none());
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }

    #[test]
    fn config_editor_canonicalizes_lm_studio_endpoint_and_rejects_url_secrets() {
        let config = ResolvedConfig::default();
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::BaseUrl)
            .expect("provider endpoint field");
        field.value = " http://lm-studio.local:1234/v1/ ".to_string();

        let patch = parse_editor_patch(&editor).expect("valid LM Studio endpoint");
        assert_eq!(
            patch.model.and_then(|model| model.base_url),
            Some("http://lm-studio.local:1234/v1".to_string())
        );

        for raw in [
            "https://user:super-secret@provider.example/v1",
            "https://provider.example/v1?api_key=hidden",
            "https://provider.example/v1#hidden",
        ] {
            let mut editor = ConfigEditorState::from_config(&config);
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::BaseUrl)
                .expect("provider endpoint field");
            field.value = raw.to_string();
            let error = parse_editor_patch(&editor).expect_err("reject secret endpoint");
            assert!(!error.contains("super-secret"));
            assert!(!error.contains("hidden"));
            assert!(!error.contains(raw));
        }
    }

    #[test]
    fn global_config_writes_never_persist_an_invalid_provider_endpoint() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original = "[model]\nbase_url = \"http://provider.example\"\n";
        std::fs::write(&path, original).expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::BaseUrl)
            .expect("provider endpoint field");
        field.value = "https://user:super-secret@provider.example/v1".to_string();
        field.dirty = true;

        let error = save_config_sections(&path, &editor).expect_err("reject invalid endpoint");

        assert!(!error.contains("super-secret"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            original
        );

        std::fs::write(
            &path,
            "[model]\nbase_url = \"https://provider.example/v1?api_key=hidden\"\n",
        )
        .expect("seed invalid existing config");
        let error = save_access_mode(&path, AccessMode::FullAccess)
            .expect_err("unrelated save cannot preserve invalid endpoint");
        assert!(!error.contains("hidden"));
        let saved = std::fs::read_to_string(&path).expect("read unchanged config");
        assert!(saved.contains("api_key=hidden"));
        assert!(!saved.contains("full_access"));
    }

    #[test]
    fn config_editor_projects_shell_hide_windows_patch() {
        let mut config = ResolvedConfig::default();
        config.shell.hide_windows = true;
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ShellHideWindows)
            .expect("shell hide window field is present");
        field.value = "false".to_string();

        let patch = parse_editor_patch(&editor).expect("shell hide_windows parses");

        assert_eq!(
            patch.shell.and_then(|shell| shell.hide_windows),
            Some(false)
        );
    }

    #[test]
    fn config_editor_global_save_preserves_unsupported_shell_fields() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[shell]\nprogram = \"pwsh\"\ndefault_timeout_ms = 777\nhide_windows = true\n",
        )
        .expect("seed existing config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let hide_windows = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ShellHideWindows)
            .expect("shell hide_windows field");
        hide_windows.value = "false".to_string();
        hide_windows.dirty = true;

        save_config_sections(&path, &editor).expect("save shell field");
        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(saved["shell"]["program"].as_str(), Some("pwsh"));
        assert_eq!(saved["shell"]["default_timeout_ms"].as_integer(), Some(777));
        assert_eq!(saved["shell"]["hide_windows"].as_bool(), Some(false));
    }

    #[test]
    fn global_save_merges_only_dirty_fields_into_current_toml() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let mut effective = ResolvedConfig::default();
        effective.model.base_url = "http://effective-env-value".to_string();
        effective.model.model = "effective-default".to_string();
        let mut editor = ConfigEditorState::from_config(&effective);

        std::fs::write(
            &path,
            "[model]\nmodel = \"external-current\"\napi_key_env = \"EXTERNAL_KEY\"\n\n[format]\nensure_trailing_newline = false\n\n[future]\nflag = \"keep\"\n",
        )
        .expect("external current config");
        let access = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::AccessMode)
            .expect("access mode field");
        access.value = "full_access".to_string();
        access.dirty = true;

        let adopted = save_config_sections(&path, &editor).expect("merge dirty config");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(adopted.model.model, "external-current");
        assert_eq!(adopted.permissions.access_mode, AccessMode::FullAccess);
        assert_eq!(saved["model"]["model"].as_str(), Some("external-current"));
        assert_eq!(saved["model"]["api_key_env"].as_str(), Some("EXTERNAL_KEY"));
        assert!(saved["model"].get("base_url").is_none());
        assert_eq!(
            saved["format"]["ensure_trailing_newline"].as_bool(),
            Some(false)
        );
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
    }

    #[test]
    fn forward_compatible_global_save_rejects_invalid_current_data_before_persist() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original =
            "[workspace]\nprotected_paths = [\"relative/path\"]\n\n[future]\nflag = \"keep\"\n";
        std::fs::write(&path, original).expect("invalid external current config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let model = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        model.value = "next-model".to_string();
        model.dirty = true;

        let error = save_config_sections(&path, &editor)
            .expect_err("known invalid data must prevent forward-compatible persistence");

        assert!(error.contains("workspace.protected_paths"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            original
        );
    }

    #[test]
    fn forward_compatible_global_save_rejects_unknown_fields_inside_current_sections() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original =
            "[model]\nmodel = \"current\"\nfuture_knob = true\n\n[future]\nflag = \"keep\"\n";
        std::fs::write(&path, original).expect("unknown current-section field");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let access = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::AccessMode)
            .expect("access mode field");
        access.value = "full_access".to_string();
        access.dirty = true;

        let error = save_config_sections(&path, &editor)
            .expect_err("unknown fields in a current section must fail closed");

        assert!(error.contains("future_knob"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            original
        );
    }

    #[test]
    fn strict_global_save_rejects_unknown_top_level_sections_before_persist() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original = "[model]\nmodel = \"current\"\n\n[future]\nflag = \"keep\"\n";
        std::fs::write(&path, original).expect("future section");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let access = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::AccessMode)
            .expect("access mode field");
        access.value = "full_access".to_string();
        access.dirty = true;

        let error = save_config_sections_resolved(&path, &editor)
            .expect_err("strict adoption must reject unknown top-level sections");

        assert!(error.contains("future"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            original
        );
    }

    #[test]
    fn global_save_without_dirty_fields_does_not_rewrite_or_pin_effective_values() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let original = "# keep formatting exactly\n[model]\nmodel='current'\n";
        std::fs::write(&path, original).expect("seed config");
        let mut effective = ResolvedConfig::default();
        effective.model.base_url = "http://env-only".to_string();
        let editor = ConfigEditorState::from_config(&effective);

        save_config_sections(&path, &editor).expect("no-op save");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read config"),
            original
        );
    }

    #[test]
    fn global_save_resolves_the_complete_candidate_before_atomic_persist() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let invalid_existing =
            "[workspace]\nprotected_paths = [\"relative/path\"]\n\n[docling]\nenabled = false\n";
        std::fs::write(&path, invalid_existing).expect("invalid existing config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let enabled = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::DoclingEnabled)
            .expect("Docling enabled field");
        enabled.value = "true".to_string();
        enabled.dirty = true;

        let error = save_config_sections_resolved(&path, &editor)
            .expect_err("the complete resulting config is invalid");

        assert!(error.contains("workspace.protected_paths"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            invalid_existing,
            "preflight failure must leave the persisted owner unchanged"
        );
    }

    #[test]
    fn global_save_returns_the_exact_resolved_config_committed_to_disk() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[docling]\nenabled = false\nbase_url = \"http://127.0.0.1:8123\"\n",
        )
        .expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        for (key, value) in [
            (ConfigField::DoclingEnabled, "true"),
            (
                ConfigField::DoclingBaseUrl,
                " https://docling.example.test/api/ ",
            ),
        ] {
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == key)
                .expect("Docling field");
            field.value = value.to_string();
            field.dirty = true;
        }

        let resolved = save_config_sections_resolved(&path, &editor).expect("atomic save");

        assert!(resolved.docling.enabled);
        assert_eq!(
            resolved.docling.base_url,
            "https://docling.example.test/api"
        );
        let saved = std::fs::read_to_string(&path).expect("saved config");
        assert!(saved.contains("enabled = true"));
        assert!(saved.contains("https://docling.example.test/api/"));
    }

    #[test]
    fn saving_request_timeout_replaces_the_legacy_stream_timeout_alias() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\nstream_idle_timeout_ms = 3600000\nmodel = \"keep-model\"\n",
        )
        .expect("legacy timeout config");
        let mut effective = ResolvedConfig::default();
        effective.model.request_timeout_ms = 3_600_000;
        effective.model.model = "keep-model".to_string();
        let mut editor = ConfigEditorState::from_config(&effective);
        let timeout = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::RequestTimeoutMs)
            .expect("canonical timeout field");
        timeout.value = "1800000".to_string();
        timeout.dirty = true;

        save_config_sections(&path, &editor).expect("save canonical timeout");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["model"]["request_timeout_ms"].as_integer(),
            Some(1_800_000)
        );
        assert!(saved["model"].get("stream_idle_timeout_ms").is_none());
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }

    #[test]
    fn request_timeout_range_is_enforced_before_apply_or_persist() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let sentinel = "[model]\nmodel = \"keep-model\"\n";
        std::fs::write(&path, sentinel).expect("config sentinel");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let timeout = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::RequestTimeoutMs)
            .expect("canonical timeout field");
        timeout.value = "3600001".to_string();
        timeout.dirty = true;

        let apply_error = editor
            .build_resolved_config(&ResolvedConfig::default())
            .expect_err("out-of-range timeout must not enter session config");
        let save_error = save_config_sections(&path, &editor)
            .expect_err("out-of-range timeout must not reach disk");

        assert!(apply_error.contains("between 1 and 3600000"));
        assert!(save_error.contains("between 1 and 3600000"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            sentinel
        );
    }

    #[test]
    fn interactive_constraints_are_rejected_before_session_commit_or_disk_write() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        let sentinel = "[model]\nmodel = \"keep-model\"\n";

        for (field, value) in [
            (ConfigField::ContextWindow, "0"),
            (ConfigField::MaxParallelPredictions, "0"),
            (ConfigField::MultiAgentMaxAgents, "0"),
            (ConfigField::MultiAgentMaxModelRequests, "0"),
            (ConfigField::Temperature, "NaN"),
            (ConfigField::TopP, "inf"),
            (ConfigField::PresencePenalty, "-inf"),
            (ConfigField::FrequencyPenalty, "NaN"),
        ] {
            std::fs::write(&path, sentinel).expect("reset config sentinel");
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let edited = editor
                .fields
                .iter_mut()
                .find(|candidate| candidate.key == field)
                .expect("interactive config field");
            edited.value = value.to_string();
            edited.dirty = true;

            let apply_error = editor
                .build_resolved_config(&ResolvedConfig::default())
                .expect_err("invalid value must not enter the session config");
            let save_error = save_config_sections(&path, &editor)
                .expect_err("invalid value must not reach the global config");

            assert!(apply_error.contains(field.label()), "{apply_error}");
            assert!(save_error.contains(field.label()), "{save_error}");
            assert_eq!(
                std::fs::read_to_string(&path).expect("unchanged config"),
                sentinel,
                "{} must fail before persistence",
                field.label(),
            );
        }
    }

    #[test]
    fn clearing_request_timeout_without_a_model_section_is_a_noop() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(&path, "[format]\nensure_trailing_newline = false\n")
            .expect("config without model section");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let timeout = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::RequestTimeoutMs)
            .expect("canonical timeout field");
        timeout.value.clear();
        timeout.dirty = true;

        save_config_sections(&path, &editor).expect("clear inherited timeout override");

        let saved = std::fs::read_to_string(&path).expect("saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert!(saved.get("model").is_none());
        assert_eq!(
            saved["format"]["ensure_trailing_newline"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn remembering_access_mode_updates_only_the_existing_permission_field() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\nmodel = \"keep-model\"\n\n[permissions]\naccess_mode = \"default\"\n\n[future]\nflag = \"keep\"\n",
        )
        .expect("seed config");

        for (mode, expected) in [
            (AccessMode::AutoReview, "auto_review"),
            (AccessMode::FullAccess, "full_access"),
            (AccessMode::Default, "default"),
        ] {
            save_access_mode(&path, mode).expect("remember access mode");

            let saved = std::fs::read_to_string(&path).expect("read saved config");
            let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
            assert_eq!(saved["permissions"]["access_mode"].as_str(), Some(expected));
            assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
            assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
        }
    }

    #[test]
    fn clearing_dirty_optional_field_removes_only_that_override() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\ntemperature = 0.7\nmodel = \"keep-model\"\n",
        )
        .expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let temperature = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::Temperature)
            .expect("temperature field");
        temperature.value.clear();
        temperature.dirty = true;

        save_config_sections(&path, &editor).expect("clear temperature override");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert!(saved["model"].get("temperature").is_none());
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }

    #[test]
    fn access_mode_compare_and_set_preserves_external_field_changes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[permissions]\naccess_mode = \"default\"\n[model]\nmodel = \"keep-model\"\n",
        )
        .expect("seed config");

        assert!(
            compare_and_set_access_mode(&path, AccessMode::Default, AccessMode::FullAccess)
                .expect("first CAS")
        );
        assert!(
            !compare_and_set_access_mode(&path, AccessMode::Default, AccessMode::Default)
                .expect("stale CAS")
        );

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(saved["model"]["model"].as_str(), Some("keep-model"));
    }

    #[test]
    fn concurrent_global_saves_preserve_each_writers_dirty_field() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(&path, "[future]\nflag = \"keep\"\n").expect("seed config");
        let barrier = Arc::new(Barrier::new(3));

        let access_path = path.clone();
        let access_barrier = Arc::clone(&barrier);
        let access_writer = std::thread::spawn(move || {
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::AccessMode)
                .expect("access mode field");
            field.value = "full_access".to_string();
            field.dirty = true;
            access_barrier.wait();
            save_config_sections(&access_path, &editor)
        });

        let model_path = path.clone();
        let model_barrier = Arc::clone(&barrier);
        let model_writer = std::thread::spawn(move || {
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == ConfigField::Model)
                .expect("model field");
            field.value = "concurrent-model".to_string();
            field.dirty = true;
            model_barrier.wait();
            save_config_sections(&model_path, &editor)
        });

        barrier.wait();
        access_writer
            .join()
            .expect("access writer")
            .expect("access save");
        model_writer
            .join()
            .expect("model writer")
            .expect("model save");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(saved["model"]["model"].as_str(), Some("concurrent-model"));
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
    }

    #[test]
    fn cross_process_global_saves_preserve_each_writers_dirty_field() {
        const CHILD_ROLE_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_ROLE";
        const CONFIG_PATH_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_PATH";
        const START_PATH_ENV: &str = "MOYAI_CONFIG_LEASE_TEST_START";
        const TEST_NAME: &str = "tui::config_editor::tests::cross_process_global_saves_preserve_each_writers_dirty_field";

        if let Ok(role) = std::env::var(CHILD_ROLE_ENV) {
            let path = Utf8PathBuf::from(
                std::env::var(CONFIG_PATH_ENV).expect("child config path environment"),
            );
            let start_path = Utf8PathBuf::from(
                std::env::var(START_PATH_ENV).expect("child start path environment"),
            );
            let ready_path = start_path.with_file_name(format!("ready-{role}"));
            std::fs::write(&ready_path, "ready").expect("child ready marker");
            wait_for_test_file(&start_path, Duration::from_secs(5));
            let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
            let (key, value) = match role.as_str() {
                "access" => (ConfigField::AccessMode, "full_access"),
                "model" => (ConfigField::Model, "cross-process-model"),
                other => panic!("unknown child role {other}"),
            };
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == key)
                .expect("child config field");
            field.value = value.to_string();
            field.dirty = true;
            for _ in 0..8 {
                save_config_sections(&path, &editor).expect("child config save");
            }
            return;
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 config path");
        let start_path =
            Utf8PathBuf::from_path_buf(temp_dir.path().join("start")).expect("utf8 start path");
        std::fs::write(&path, "[future]\nflag = \"keep\"\n").expect("seed config");
        let executable = std::env::current_exe().expect("current test executable");
        let mut children = ["access", "model"].map(|role| {
            Command::new(&executable)
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .env(CHILD_ROLE_ENV, role)
                .env(CONFIG_PATH_ENV, path.as_str())
                .env(START_PATH_ENV, start_path.as_str())
                .spawn()
                .expect("spawn config writer child")
        });
        for role in ["access", "model"] {
            wait_for_test_file(
                &start_path.with_file_name(format!("ready-{role}")),
                Duration::from_secs(5),
            );
        }
        std::fs::write(&start_path, "start").expect("release config writers");
        for child in &mut children {
            let status = child.wait().expect("config writer child status");
            assert!(status.success(), "config writer child failed: {status}");
        }

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(
            saved["permissions"]["access_mode"].as_str(),
            Some("full_access")
        );
        assert_eq!(
            saved["model"]["model"].as_str(),
            Some("cross-process-model")
        );
        assert_eq!(saved["future"]["flag"].as_str(), Some("keep"));
    }

    fn wait_for_test_file(path: &camino::Utf8Path, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
