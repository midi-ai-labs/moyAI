use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::NamedTempFile;

use crate::config::field::{build_resolved_config_from_field_values, parse_config_field_patch};
use crate::config::loader::{
    acquire_global_config_write_lease, global_config_path, read_toml_utf8_bounded,
};
use crate::config::model::{AccessMode, ApproveConfig, ResolvedConfig};
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

#[derive(Clone)]
pub struct ConfigFieldState {
    pub key: ConfigField,
    pub value: String,
    pub dirty: bool,
}

impl std::fmt::Debug for ConfigFieldState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let debug_value = if matches!(
            self.key,
            ConfigField::SystemPrompt | ConfigField::SideChatSystemPrompt
        ) {
            format!("<{} chars>", self.value.chars().count())
        } else if self.key.is_sensitive() {
            self.value
                .is_empty()
                .then_some("<not configured>")
                .unwrap_or("<redacted; configured>")
                .to_string()
        } else {
            self.value.clone()
        };
        formatter
            .debug_struct("ConfigFieldState")
            .field("key", &self.key)
            .field("value", &debug_value)
            .field("dirty", &self.dirty)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ConfigEditorState {
    pub fields: Vec<ConfigFieldState>,
    pub selected: usize,
    pub feedback: Option<String>,
    imported_approve: Option<ApproveConfig>,
}

impl ConfigEditorState {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        Self {
            fields: ConfigField::ALL
                .into_iter()
                .map(|key| ConfigFieldState {
                    key,
                    value: key.editor_value(config),
                    dirty: false,
                })
                .collect(),
            selected: 0,
            feedback: None,
            imported_approve: None,
        }
    }

    pub fn from_tui_config(config: &ResolvedConfig) -> Self {
        let mut editor = Self::from_config(config);
        editor
            .fields
            .retain(|field| !field.key.is_host_owned_generation());
        editor
    }

    pub(crate) fn with_imported_approve(mut self, approve: ApproveConfig) -> Self {
        self.imported_approve = Some(approve);
        self
    }

    pub fn from_config_values(
        config: &ResolvedConfig,
        values: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let mut candidate = Self::from_config(config);
        candidate.replace_values_by_key(values)?;
        Ok(candidate)
    }

    pub fn from_complete_config_values(
        config: &ResolvedConfig,
        values: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let keys = values
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut candidate = Self::from_config(config);
        let explicit_private = candidate
            .fields
            .iter()
            .filter(|field| {
                !field.key.is_approve()
                    && field.key.is_connection_private_value()
                    && values.iter().any(|(key, value)| {
                        key == field.key.label()
                            && !field
                                .key
                                .redacted_input_preserves_configured(&field.value, value)
                    })
            })
            .map(|field| field.key)
            .collect::<std::collections::HashSet<_>>();
        candidate.replace_values_by_key(values)?;
        for field in &mut candidate.fields {
            if !field.key.is_approve()
                && !field.key.is_connection_private_value()
                && keys.contains(field.key.label())
                || explicit_private.contains(&field.key)
            {
                field.dirty = true;
            }
        }
        Ok(candidate)
    }

    pub fn replace_values_by_key(&mut self, values: Vec<(String, String)>) -> Result<(), String> {
        let connections = [
            (
                ConfigField::BaseUrl,
                ConfigField::ProviderProfile,
                ConfigField::ApiKeyEnv,
            ),
            (
                ConfigField::SideChatBaseUrl,
                ConfigField::SideChatProviderProfile,
                ConfigField::SideChatApiKeyEnv,
            ),
            (
                ConfigField::ApproveBaseUrl,
                ConfigField::ApproveProviderProfile,
                ConfigField::ApproveApiKeyEnv,
            ),
        ]
        .map(|(url, profile, credential)| {
            let value = |key| {
                self.fields
                    .iter()
                    .find(|field| field.key == key)
                    .map(|field| field.value.clone())
            };
            (url, profile, credential, value(url), value(profile))
        });
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
            if field
                .key
                .redacted_input_preserves_configured(&field.value, &value)
            {
                continue;
            }
            field.dirty = field.value != value;
            field.value = value;
        }
        for (url, profile, credential, previous_url, previous_profile) in connections {
            let changed = self.fields.iter().any(|field| {
                if field.key == url {
                    ProviderEndpoint::parse(&field.value).ok()
                        != previous_url
                            .as_deref()
                            .and_then(|value| ProviderEndpoint::parse(value).ok())
                } else {
                    field.key == profile && Some(&field.value) != previous_profile.as_ref()
                }
            });
            if changed && seen.contains(credential.label()) {
                if let Some(field) = self.fields.iter_mut().find(|field| field.key == credential) {
                    field.dirty = true;
                }
            }
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
            .filter(|field| {
                (!field.key.is_approve() && !field.key.is_connection_private_value()) || field.dirty
            })
            .map(|field| (field.key, field.value.as_str()))
            .collect::<Vec<_>>();
        let mut base = base.clone();
        if let Some(approve) = &self.imported_approve {
            base.approve = Some(approve.clone());
        }
        build_resolved_config_from_field_values(&base, &fields)
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

/// Public Hub trust is imported independently of model settings, secrets and
/// local workspace authority, under the same global config write lease.
pub(crate) fn save_device_network_config(
    path: &Utf8Path,
    expected: &crate::device_network::SharedHubConfig,
    shared: &crate::device_network::SharedHubConfig,
    validate: impl FnOnce(&ResolvedConfig) -> Result<(), String>,
) -> Result<ResolvedConfig, String> {
    // An entirely empty public connection is the explicit local reset result.
    // Partial connection documents are still invalid.
    if shared.configured() {
        shared
            .validate()
            .map_err(|_| "invalid_configuration".to_string())?;
    }
    let _lease =
        acquire_global_config_write_lease(path).map_err(|_| "storage_error".to_string())?;
    let mut document = read_toml_document(path).map_err(|_| "settings_corrupt".to_string())?;
    let current: crate::device_network::SharedHubConfig = document
        .get("device_network")
        .map(|value| value.clone().try_into())
        .transpose()
        .map_err(|_| "settings_corrupt".to_string())?
        .unwrap_or_default();
    if &current != expected {
        return Err("connection_changed".into());
    }
    document.as_table_mut().ok_or("settings_corrupt")?.insert(
        "device_network".into(),
        toml::Value::try_from(shared).map_err(|_| "invalid_configuration".to_string())?,
    );
    let text = toml::to_string_pretty(&document).map_err(|_| "storage_error".to_string())?;
    let resolved =
        ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(path, &text)
            .map_err(|_| "invalid_configuration".to_string())?
            .resolved_config;
    validate(&resolved)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "storage_error".to_string())?;
    }
    persist_config_tempfile(path, &text).map_err(|_| "storage_error".to_string())?;
    Ok(resolved)
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
    let dirty = !dirty_values.is_empty() || editor.imported_approve.is_some();
    let mut existing = read_toml_document(path)?;
    if dirty {
        let text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
        let previous =
            ConfigLoader::resolve_forward_compatible_global_config_text_without_environment(
                path, &text,
            )
            .map_err(|error| error.to_string())?;
        let previous_approve = previous.approve.clone();
        let patch = parse_config_field_patch(&dirty_values)?;
        let mut resolved = crate::config::merge::apply_patch(previous.clone(), patch.clone());
        resolved.normalize_and_validate_provider_runtime()?;
        let patch = toml::Value::try_from(patch).map_err(|error| error.to_string())?;
        for (field, _) in &dirty_values {
            if !field.is_approve() {
                apply_dirty_toml_field(&mut existing, &patch, *field)?;
            }
        }
        let main_changed = ProviderEndpoint::parse(&previous.model.base_url).ok()
            != ProviderEndpoint::parse(&resolved.model.base_url).ok()
            || previous.model.provider_profile != resolved.model.provider_profile;
        let side_changed = ProviderEndpoint::parse(&previous.side_chat.base_url).ok()
            != ProviderEndpoint::parse(&resolved.side_chat.base_url).ok()
            || previous.side_chat.provider_profile != resolved.side_chat.provider_profile;
        for (changed, section_name, api_key_env) in [
            (main_changed, "model", &resolved.model.api_key_env),
            (side_changed, "side_chat", &resolved.side_chat.api_key_env),
        ] {
            if changed {
                let section = existing
                    .as_table_mut()
                    .expect("TOML root is a table")
                    .entry(section_name.to_string())
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .ok_or_else(|| {
                        format!("global config section `{section_name}` must be a TOML table")
                    })?;
                // Main and Sub have no inherited credential default. Remove the
                // old reference when the new target has none.
                if let Some(reference) = api_key_env {
                    section.insert(
                        "api_key_env".to_string(),
                        toml::Value::String(reference.clone()),
                    );
                } else {
                    section.remove("api_key_env");
                }
                if section_name == "model" {
                    section.insert(
                        "extra_headers".to_string(),
                        toml::Value::try_from(&resolved.model.extra_headers)
                            .map_err(|error| error.to_string())?,
                    );
                    if let Some(body) = &resolved.model.extra_body_json {
                        section.insert(
                            "extra_body_json".to_string(),
                            toml::Value::try_from(body).map_err(|error| error.to_string())?,
                        );
                    } else {
                        section.remove("extra_body_json");
                    }
                }
            }
        }
        let approve_values = dirty_values
            .iter()
            .copied()
            .filter(|(field, _)| field.is_approve())
            .collect::<Vec<_>>();
        if !approve_values.is_empty()
            || editor.imported_approve.is_some()
            || previous_approve.is_some()
        {
            let base_text = toml::to_string_pretty(&existing).map_err(|error| error.to_string())?;
            let mut base =
                ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
                    path, &base_text,
                )
                .map_err(|error| error.to_string())?
                .resolved_config;
            if let Some(approve) = previous_approve {
                // Materialize older partial sections before a Main edit can
                // change the independent approval connection on reload.
                base.approve = Some(approve);
            }
            if let Some(approve) = &editor.imported_approve {
                base.approve = Some(approve.clone());
            }
            let resolved = build_resolved_config_from_field_values(&base, &approve_values)?;
            if let Some(approve) = resolved.approve {
                let mut snapshot =
                    toml::Value::try_from(&approve).map_err(|error| error.to_string())?;
                // An omitted credential reference would inherit Main when this
                // independent snapshot is loaded into a fresh config owner.
                snapshot
                    .as_table_mut()
                    .expect("Approve serializes as a table")
                    .insert(
                        "api_key_env".to_string(),
                        toml::Value::String(approve.api_key_env.unwrap_or_default()),
                    );
                existing
                    .as_table_mut()
                    .expect("TOML root is a table")
                    .insert("approve".to_string(), snapshot);
            }
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
    use crate::config::{AccessMode, ConfigLoader, ProviderProfile, ResolvedConfig};

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

    fn approval_save_fixture(path: &camino::Utf8Path) -> ResolvedConfig {
        std::fs::write(path, "[model]\nbase_url='https://main.example/v1'\nmodel='main-before'\nprovider_profile='openai_compatible'\napi_key_env='MAIN_TEST_KEY'\ncontext_window=32768\nrequest_timeout_ms=4000\nconnect_timeout_ms=1500\nmax_retries=2\n[model.extra_headers]\nX-Test='main-header'\n").unwrap();
        load_saved_config(path)
    }

    fn load_saved_config(path: &camino::Utf8Path) -> ResolvedConfig {
        crate::config::ConfigLoader::resolve_global_config_text_without_environment(
            path,
            &std::fs::read_to_string(path).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn approval_inheritance_survives_complete_setup_and_unrelated_editor_saves() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let base = approval_save_fixture(&path);
        let values = ConfigField::ALL
            .into_iter()
            .map(|key| (key.label().to_string(), key.editor_value(&base)))
            .collect();
        let setup = ConfigEditorState::from_complete_config_values(&base, values).unwrap();
        assert!(
            setup
                .build_resolved_config(&base)
                .unwrap()
                .approve
                .is_none()
        );
        save_config_sections_resolved(&path, &setup).unwrap();
        assert!(load_saved_config(&path).approve.is_none());
        for key in [ConfigField::Model, ConfigField::DoclingEnabled] {
            let base = load_saved_config(&path);
            let value = if key == ConfigField::Model {
                "main-after"
            } else {
                "false"
            };
            let mut editor = ConfigEditorState::from_tui_config(&base);
            let field = editor
                .fields
                .iter_mut()
                .find(|field| field.key == key)
                .unwrap();
            field.value = value.to_string();
            field.dirty = true;
            assert!(
                editor
                    .build_resolved_config(&base)
                    .unwrap()
                    .approve
                    .is_none()
            );
            save_config_sections_resolved(&path, &editor).unwrap();
            assert!(load_saved_config(&path).approve.is_none());
        }
    }

    #[test]
    fn first_approval_edit_persists_a_complete_independent_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let base = approval_save_fixture(&path);
        let editor = ConfigEditorState::from_config_values(
            &base,
            vec![("approve.model".into(), "fast-judge".into())],
        )
        .unwrap();
        let committed = save_config_sections_resolved(&path, &editor).unwrap();
        let snapshot = committed.approve.clone().unwrap();
        assert_eq!(snapshot.model, "fast-judge");
        assert_eq!(snapshot.api_key_env, base.model.api_key_env);
        assert_eq!(snapshot.extra_headers, base.model.extra_headers);
        let document: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(document["approve"].as_table().unwrap().len(), 9);
        let editor = ConfigEditorState::from_config_values(
            &committed,
            vec![
                (
                    "model.base_url".into(),
                    "https://other-main.example/v1".into(),
                ),
                ("model.model".into(), "main-after".into()),
                ("model.api_key_env".into(), "ANOTHER_MAIN_KEY".into()),
                ("model.request_timeout_ms".into(), "9999".into()),
            ],
        )
        .unwrap();
        save_config_sections_resolved(&path, &editor).unwrap();
        assert_eq!(load_saved_config(&path).approve, Some(snapshot));
    }

    #[test]
    fn main_save_materializes_an_existing_partial_approval_before_main_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        approval_save_fixture(&path);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[approve]\nmodel='old-partial-judge'\n");
        std::fs::write(&path, text).unwrap();
        let base = load_saved_config(&path);
        let snapshot = base.approve.clone().unwrap();
        let editor = ConfigEditorState::from_config_values(
            &base,
            vec![
                (
                    "model.base_url".into(),
                    "https://new-main.example/v1".into(),
                ),
                ("model.api_key_env".into(), "NEW_MAIN_KEY".into()),
                ("model.extra_headers_json".into(), "{}".into()),
                ("model.context_window".into(), "65536".into()),
            ],
        )
        .unwrap();
        save_config_sections_resolved(&path, &editor).unwrap();
        let loaded = load_saved_config(&path);
        assert_eq!(loaded.approve, Some(snapshot));
        assert_eq!(loaded.model.base_url, "https://new-main.example/v1");
        let document: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(document["approve"].as_table().unwrap().len(), 9);
    }

    #[test]
    fn approval_endpoint_and_profile_changes_clear_credentials_after_save_and_reload() {
        for changed_field in [
            ConfigField::ApproveBaseUrl,
            ConfigField::ApproveProviderProfile,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
            let base = approval_save_fixture(&path);
            let editor = ConfigEditorState::from_config_values(
                &base,
                vec![("approve.model".into(), "fast-judge".into())],
            )
            .unwrap();
            let base = save_config_sections_resolved(&path, &editor).unwrap();
            let value = if changed_field == ConfigField::ApproveBaseUrl {
                "https://other-judge.example/v1"
            } else {
                "lm_studio"
            };
            let editor = ConfigEditorState::from_config_values(
                &base,
                vec![(changed_field.label().into(), value.into())],
            )
            .unwrap();
            let applied = editor.build_resolved_config(&base).unwrap();
            assert_eq!(applied.approve.as_ref().unwrap().api_key_env, None);
            assert!(applied.approve.as_ref().unwrap().extra_headers.is_empty());
            save_config_sections_resolved(&path, &editor).unwrap();
            let loaded = load_saved_config(&path);
            assert_eq!(loaded.approve.as_ref().unwrap().api_key_env, None);
            assert!(loaded.approve.as_ref().unwrap().extra_headers.is_empty());
            assert_eq!(loaded.model.api_key_env.as_deref(), Some("MAIN_TEST_KEY"));
            let document: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(document["approve"]["api_key_env"].as_str(), Some(""));
        }
    }

    #[test]
    fn approval_explicit_same_key_and_anonymous_replacement_survive_reload() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let base = approval_save_fixture(&path);
        let editor = ConfigEditorState::from_config_values(
            &base,
            vec![
                ("approve.base_url".into(), "https://judge.example/v1".into()),
                ("approve.api_key_env".into(), "MAIN_TEST_KEY".into()),
            ],
        )
        .unwrap();
        save_config_sections_resolved(&path, &editor).unwrap();
        let base = load_saved_config(&path);
        assert_eq!(
            base.approve.as_ref().unwrap().api_key_env.as_deref(),
            Some("MAIN_TEST_KEY")
        );
        assert!(base.approve.as_ref().unwrap().extra_headers.is_empty());
        let editor = ConfigEditorState::from_config_values(
            &base,
            vec![("approve.api_key_env".into(), "".into())],
        )
        .unwrap();
        save_config_sections_resolved(&path, &editor).unwrap();
        assert_eq!(
            load_saved_config(&path)
                .approve
                .as_ref()
                .unwrap()
                .api_key_env,
            None
        );
    }

    #[test]
    fn main_and_sub_connection_edits_clear_private_values_after_apply_save_and_reload() {
        for role in ["model", "side_chat"] {
            for (field, value) in [
                ("base_url", "https://new.example/v1"),
                ("provider_profile", "lm_studio"),
            ] {
                let temp = tempfile::tempdir().unwrap();
                let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
                std::fs::write(&path, "[model]\nbase_url='https://old.example/v1'\nprovider_profile='openai_compatible'\napi_key_env='OLD_MAIN_KEY'\n[model.extra_headers]\nAuthorization='Bearer old-secret'\n[side_chat]\nbase_url='https://old.example/v1'\nprovider_profile='openai_compatible'\napi_key_env='OLD_SUB_KEY'\n[future]\nretained=true\n").unwrap();
                let base = ConfigLoader::resolve_forward_compatible_global_config_text_without_environment(&path, &std::fs::read_to_string(&path).unwrap()).unwrap();
                let editor = ConfigEditorState::from_config_values(
                    &base,
                    vec![(format!("{role}.{field}"), value.into())],
                )
                .unwrap();
                let applied = editor.build_resolved_config(&base).unwrap();
                if role == "model" {
                    assert_eq!(applied.model.api_key_env, None);
                    assert!(applied.model.extra_headers.is_empty());
                } else {
                    assert_eq!(applied.side_chat.api_key_env, None);
                }
                save_config_sections(&path, &editor).unwrap();
                let reopened = ConfigLoader::resolve_forward_compatible_global_config_text_without_environment(&path, &std::fs::read_to_string(&path).unwrap()).unwrap();
                if role == "model" {
                    assert_eq!(reopened.model.api_key_env, None);
                    assert!(reopened.model.extra_headers.is_empty());
                    assert_eq!(reopened.side_chat.api_key_env, base.side_chat.api_key_env);
                } else {
                    assert_eq!(reopened.side_chat.api_key_env, None);
                    assert_eq!(reopened.model.api_key_env, base.model.api_key_env);
                    assert_eq!(reopened.model.extra_headers, base.model.extra_headers);
                }
                let document: toml::Value =
                    toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                assert_eq!(document["future"]["retained"].as_bool(), Some(true));
                assert!(document[role].get("api_key_env").is_none());
            }
        }
    }

    #[test]
    fn imported_approval_equal_to_main_keeps_explicit_ownership_and_private_headers() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let base = approval_save_fixture(&path);
        let snapshot = crate::config::ApproveConfig::from_model(&base.model);
        let values = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_approve())
            .map(|key| (key.label().into(), key.editor_value(&base)))
            .collect();
        let editor = ConfigEditorState::from_complete_config_values(&base, values)
            .unwrap()
            .with_imported_approve(snapshot.clone());
        assert_eq!(
            editor.build_resolved_config(&base).unwrap().approve,
            Some(snapshot.clone())
        );
        save_config_sections_resolved(&path, &editor).unwrap();
        assert_eq!(load_saved_config(&path).approve, Some(snapshot));
        let base = load_saved_config(&path);
        let later = ConfigEditorState::from_config_values(
            &base,
            vec![("approve.model".into(), "later-judge".into())],
        )
        .unwrap();
        save_config_sections_resolved(&path, &later).unwrap();
        let base = load_saved_config(&path);
        let unrelated = ConfigEditorState::from_config_values(
            &base,
            vec![("model.model".into(), "later-main".into())],
        )
        .unwrap();
        save_config_sections_resolved(&path, &unrelated).unwrap();
        assert_eq!(
            load_saved_config(&path).approve.as_ref().unwrap().model,
            "later-judge"
        );
    }

    fn shared_hub_config() -> crate::device_network::SharedHubConfig {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        crate::device_network::SharedHubConfig {
            hub_url: "https://127.0.0.1:8443".into(),
            ca_certificate_pem: params.self_signed(&key).unwrap().pem(),
        }
    }

    #[test]
    fn device_network_import_preserves_local_settings_and_rejects_stale_trust() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let original = "[model]\nmodel = 'local-choice'\n[permissions]\naccess_mode = 'default'\n[future_integration]\nlocal_secret = 'retain-this-locally'\n";
        std::fs::write(&path, original).unwrap();
        let shared = shared_hub_config();
        let resolved =
            super::save_device_network_config(&path, &Default::default(), &shared, |_| Ok(()))
                .unwrap();
        assert_eq!(resolved.device_network, shared);
        assert_eq!(resolved.model.model, "local-choice");
        let saved = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&saved).unwrap();
        assert_eq!(
            parsed["future_integration"]["local_secret"].as_str(),
            Some("retain-this-locally")
        );
        assert_eq!(
            parsed["permissions"]["access_mode"].as_str(),
            Some("default")
        );
        assert_eq!(parsed["device_network"].as_table().unwrap().len(), 2);
        assert_eq!(
            super::save_device_network_config(
                &path,
                &Default::default(),
                &shared_hub_config(),
                |_| Ok(()),
            )
            .unwrap_err(),
            "connection_changed"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    }

    #[test]
    fn device_network_import_validates_before_persisting_or_creating_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        let shared = shared_hub_config();
        assert_eq!(
            super::save_device_network_config(&path, &Default::default(), &shared, |_| Err(
                "initial_setup_incomplete".into()
            ),)
            .unwrap_err(),
            "initial_setup_incomplete"
        );
        assert!(!path.exists());
        std::fs::write(&path, "[model]\nmodel = 'preserved'\n").unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut invalid = shared.clone();
        invalid.hub_url = "http://127.0.0.1:8443".into();
        assert_eq!(
            super::save_device_network_config(&path, &Default::default(), &invalid, |_| Ok(()),)
                .unwrap_err(),
            "invalid_configuration"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        super::save_device_network_config(&path, &Default::default(), &shared, |_| Ok(())).unwrap();
    }

    #[test]
    fn local_connection_reset_preserves_direct_model_and_unrelated_settings() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
        std::fs::write(
            &path,
            "[model]\nmodel='saved-direct-model'\n[future_integration]\nlocal_secret='unchanged'\n",
        )
        .unwrap();
        let shared = shared_hub_config();
        super::save_device_network_config(&path, &Default::default(), &shared, |_| Ok(())).unwrap();
        let cleared =
            super::save_device_network_config(&path, &shared, &Default::default(), |_| Ok(()))
                .unwrap();
        assert!(!cleared.device_network.configured());
        assert_eq!(cleared.model.model, "saved-direct-model");
        let document: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            document["future_integration"]["local_secret"].as_str(),
            Some("unchanged")
        );
        let mut partial = crate::device_network::SharedHubConfig::default();
        partial.hub_url = "https://new.example".into();
        assert_eq!(
            super::save_device_network_config(&path, &Default::default(), &partial, |_| Ok(()))
                .unwrap_err(),
            "invalid_configuration"
        );
    }

    #[test]
    fn config_editor_excludes_host_owned_generation_settings() {
        let editor = ConfigEditorState::from_tui_config(&ResolvedConfig::default());
        let labels = editor
            .fields
            .iter()
            .map(|field| field.key.label())
            .collect::<Vec<_>>();

        assert!(!labels.contains(&"model.prompt_profile"));
        assert!(!labels.contains(&"session.max_steps_per_turn"));
        for field in ConfigField::ALL
            .into_iter()
            .filter(|field| field.is_host_owned_generation())
        {
            assert!(
                !labels.contains(&field.label()),
                "{} must not be editable in the TUI",
                field.label()
            );
        }
        for absent_legacy_key in [
            "model.reasoning_effort",
            "model.reasoning_summary",
            "model.chat_completions_reasoning_parameters",
        ] {
            assert!(
                !labels.contains(&absent_legacy_key),
                "{absent_legacy_key} must not become a TUI field"
            );
        }
    }

    #[test]
    fn raw_editor_values_remain_mutable_but_debug_is_credential_safe() {
        let secret = "editor-header-super-secret";
        let main_prompt = "editor-main-system-prompt-secret";
        let side_prompt = "editor-side-system-prompt-secret";
        let mut config = ResolvedConfig::default();
        config.model.system_prompt = main_prompt.to_string();
        config.side_chat.system_prompt = side_prompt.to_string();
        config
            .model
            .extra_headers
            .insert("Authorization".to_string(), secret.to_string());
        let mut editor = ConfigEditorState::from_config(&config);
        let field = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::ExtraHeadersJson)
            .expect("sensitive raw editor field");

        assert!(field.value.contains(secret));
        field.value = format!(r#"{{"Authorization":"{secret}-changed"}}"#);
        field.dirty = true;
        let debug = format!("{editor:?}");

        assert!(!debug.contains(secret));
        assert!(!debug.contains(main_prompt));
        assert!(!debug.contains(side_prompt));
        assert!(debug.contains("<redacted; configured>"));
        assert!(debug.contains(&format!("<{} chars>", main_prompt.chars().count())));
        assert!(debug.contains(&format!("<{} chars>", side_prompt.chars().count())));

        let redacted = ConfigEditorState::from_config_values(
            &config,
            vec![(
                ConfigField::ExtraHeadersJson.label().to_string(),
                String::new(),
            )],
        )
        .expect("redacted public input");
        let preserved = redacted
            .fields
            .iter()
            .find(|field| field.key == ConfigField::ExtraHeadersJson)
            .expect("preserved sensitive editor field");
        assert!(preserved.value.contains(secret));
        assert!(!preserved.dirty);

        let explicit_clear = ConfigEditorState::from_config_values(
            &config,
            vec![(
                ConfigField::ExtraHeadersJson.label().to_string(),
                "{}".to_string(),
            )],
        )
        .expect("explicit sensitive replacement");
        let cleared = explicit_clear
            .fields
            .iter()
            .find(|field| field.key == ConfigField::ExtraHeadersJson)
            .expect("replaced sensitive editor field");
        assert_eq!(cleared.value, "{}");
        assert!(cleared.dirty);
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
        assert_eq!(
            response_timeout.key.display_label(),
            "LLM response inactivity timeout"
        );
        assert!(
            response_timeout
                .key
                .help()
                .contains("SSE event間の最大無進捗時間")
        );
        assert!(response_timeout.key.help().contains("総所要時間は制限せず"));
        assert!(response_timeout.key.help().contains("hostへも送信しません"));
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
    fn complete_session_candidate_discards_hidden_host_generation_values() {
        let mut base = ResolvedConfig::default();
        base.model.temperature = Some(0.7);
        base.model.extra_body_json = Some(serde_json::json!({"num_ctx": 32768}));
        let candidate = ConfigEditorState::from_config_values(
            &base,
            vec![(
                ConfigField::Model.label().to_string(),
                "changed-model".to_string(),
            )],
        )
        .expect("complete config values");

        let resolved = candidate
            .build_resolved_config(&base)
            .expect("complete config candidate");

        assert_eq!(resolved.model.temperature, None);
        assert_eq!(resolved.model.extra_body_json, None);
        assert_eq!(resolved.model.model, "changed-model");
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
    fn main_and_sub_save_preserve_same_target_and_explicit_reentered_credentials() {
        for (role, reference) in [("model", "MAIN_KEY"), ("side_chat", "SUB_KEY")] {
            let temp_dir = tempfile::tempdir().unwrap();
            let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml")).unwrap();
            let seed = format!(
                "[{role}]\nbase_url = \"https://old.example.test/v1\"\nprovider_profile = \"openai_compatible\"\napi_key_env = \"{reference}\"\n"
            );
            std::fs::write(&path, &seed).unwrap();
            let base =
                ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
                    &path, &seed,
                )
                .unwrap()
                .resolved_config;
            let same = ConfigEditorState::from_config_values(
                &base,
                vec![(format!("{role}.model"), "another-model".into())],
            )
            .unwrap();
            let saved = save_config_sections(&path, &same).unwrap();
            let reference_of = |config: &ResolvedConfig| {
                if role == "model" {
                    config.model.api_key_env.clone()
                } else {
                    config.side_chat.api_key_env.clone()
                }
            };
            assert_eq!(reference_of(&saved).as_deref(), Some(reference));
            let explicit = ConfigEditorState::from_complete_config_values(
                &saved,
                vec![
                    (
                        format!("{role}.base_url"),
                        "https://new.example.test/v1".into(),
                    ),
                    (format!("{role}.api_key_env"), reference.into()),
                ],
            )
            .unwrap();
            let applied = explicit.build_resolved_config(&saved).unwrap();
            assert_eq!(reference_of(&applied).as_deref(), Some(reference));
            let saved = save_config_sections(&path, &explicit).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let reopened =
                ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
                    &path, &text,
                )
                .unwrap()
                .resolved_config;
            assert_eq!(reference_of(&saved).as_deref(), Some(reference));
            assert_eq!(reference_of(&reopened).as_deref(), Some(reference));
        }
    }

    #[test]
    fn complete_config_values_persist_even_when_they_match_environment_effective_values() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\nbase_url = \"http://127.0.0.1:1234\"\nmodel = \"persisted-default\"\nprovider_profile = \"lm_studio\"\n",
        )
        .expect("seed persisted defaults");
        let mut effective = ResolvedConfig::default();
        effective.model.base_url = "https://provider.example.test/v1".to_string();
        effective.model.model = "environment-model".to_string();
        effective.model.provider_profile = ProviderProfile::OpenAiCompatible;
        effective.model.extra_headers.insert(
            "Authorization".to_string(),
            "COMPLETE_CONFIG_SECRET".to_string(),
        );
        let values = [
            ConfigField::BaseUrl,
            ConfigField::Model,
            ConfigField::ProviderProfile,
            ConfigField::ExtraHeadersJson,
        ]
        .into_iter()
        .map(|field| (field.label().to_string(), field.editor_value(&effective)))
        .collect();
        let editor = ConfigEditorState::from_complete_config_values(&effective, values)
            .expect("complete values");

        let resolved = save_config_sections_resolved(&path, &editor).expect("complete save");

        assert_eq!(resolved.model.base_url, effective.model.base_url);
        assert_eq!(resolved.model.model, effective.model.model);
        assert_eq!(
            resolved.model.provider_profile,
            ProviderProfile::OpenAiCompatible
        );
        assert_eq!(resolved.model.extra_headers, effective.model.extra_headers);
        let saved = std::fs::read_to_string(&path).expect("saved config");
        assert!(saved.contains("https://provider.example.test/v1"));
        assert!(saved.contains("environment-model"));
        assert!(saved.contains("openai_compatible"));
        assert!(saved.contains("COMPLETE_CONFIG_SECRET"));
    }

    #[test]
    fn environment_connection_materialization_does_not_restore_old_credentials() {
        const CHILD_MODE: &str = "MOYAI_CONFIG_CREDENTIAL_MATERIALIZATION_TEST";
        const TEST_NAME: &str = "tui::config_editor::tests::environment_connection_materialization_does_not_restore_old_credentials";
        if let Ok(mode) = std::env::var(CHILD_MODE) {
            let temp = tempfile::tempdir().unwrap();
            let path = Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).unwrap();
            let seed = "[model]\nbase_url='https://old.example.test/v1'\nprovider_profile='openai_compatible'\napi_key_env='OLD_KEY'\nextra_body_json={token='old-secret'}\n[model.extra_headers]\nAuthorization='Bearer old-secret'\n[future]\nretained=true\n";
            std::fs::write(&path, seed).unwrap();
            let effective =
                ConfigLoader::resolve_forward_compatible_global_config_text_with_environment(
                    &path, seed,
                )
                .unwrap()
                .resolved_config;
            assert!(effective.model.api_key_env.is_none());
            assert!(effective.model.extra_headers.is_empty());
            let editor = if mode == "model" {
                ConfigEditorState::from_config_values(
                    &effective,
                    vec![("model.model".into(), "saved-model".into())],
                )
                .unwrap()
            } else {
                ConfigEditorState::from_complete_config_values(
                    &effective,
                    vec![
                        ("model.base_url".into(), effective.model.base_url.clone()),
                        (
                            "model.provider_profile".into(),
                            effective.model.provider_profile.as_str().into(),
                        ),
                        ("model.model".into(), "saved-model".into()),
                    ],
                )
                .unwrap()
            };
            let saved = save_config_sections(&path, &editor).unwrap();
            assert!(saved.model.api_key_env.is_none());
            assert!(saved.model.extra_headers.is_empty());
            let text = std::fs::read_to_string(&path).unwrap();
            let reopened =
                ConfigLoader::resolve_forward_compatible_global_config_text_without_environment(
                    &path, &text,
                )
                .unwrap();
            let document: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(document["future"]["retained"].as_bool(), Some(true));
            if mode == "model" {
                assert_eq!(reopened.model.base_url, "https://old.example.test/v1");
                assert_eq!(reopened.model.api_key_env.as_deref(), Some("OLD_KEY"));
                assert_eq!(
                    reopened
                        .model
                        .extra_headers
                        .get("Authorization")
                        .map(String::as_str),
                    Some("Bearer old-secret")
                );
            } else {
                assert!(
                    reopened.model.api_key_env.is_none(),
                    "materializing the effective target must not restore the old key"
                );
                assert!(reopened.model.extra_headers.is_empty());
                assert!(document["model"].get("extra_body_json").is_none());
            }
            return;
        }
        let executable = std::env::current_exe().unwrap();
        for mode in ["url", "profile", "model"] {
            let mut child = Command::new(&executable);
            child
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(CHILD_MODE, mode);
            for key in ConfigField::ALL
                .into_iter()
                .filter_map(ConfigField::env_override)
            {
                child.env_remove(key);
            }
            child
                .env_remove("MOYAI_PROVIDER_METADATA_MODE")
                .env_remove("MOYAI_PROVIDER_API_MODE");
            if mode == "profile" {
                child.env("MOYAI_PROVIDER_PROFILE", "openai_responses");
            } else {
                child.env("MOYAI_BASE_URL", "https://new.example.test/v1");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{mode} child failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
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
    fn saving_visible_field_preserves_hidden_generation_override() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp_dir.path().join("config.toml"))
            .expect("utf8 temp path");
        std::fs::write(
            &path,
            "[model]\ntemperature = 0.7\nmodel = \"keep-model\"\n",
        )
        .expect("seed config");
        let mut editor = ConfigEditorState::from_config(&ResolvedConfig::default());
        let model = editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        model.value = "changed-model".to_string();
        model.dirty = true;

        save_config_sections(&path, &editor).expect("save visible field");

        let saved = std::fs::read_to_string(&path).expect("read saved config");
        let saved: toml::Value = toml::from_str(&saved).expect("parse saved config");
        assert_eq!(saved["model"]["temperature"].as_float(), Some(0.7));
        assert_eq!(saved["model"]["model"].as_str(), Some("changed-model"));
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
