use super::*;

impl DesktopController {
    pub(crate) fn pick_global_config_toml_dialog(&mut self) -> Option<Utf8PathBuf> {
        match pick_config_toml_file(None) {
            Ok(path) => path,
            Err(error) => {
                self.state.set_typed_status_message(
                    DesktopStatusCode::ConfigImportFailed,
                    format!("config import failed: {error}"),
                );
                None
            }
        }
    }

    pub(crate) fn pick_initial_setup_config_toml_dialog(
        start_dir: &Utf8Path,
    ) -> Result<Option<Utf8PathBuf>, String> {
        pick_config_toml_file(Some(start_dir))
    }

    pub(crate) fn load_initial_setup_config_toml_path(
        path: &Utf8Path,
    ) -> Result<ResolvedConfig, String> {
        validate_import_config_extension(path)?;
        let text = read_toml_utf8_bounded(path).map_err(public_config_import_error)?;
        ConfigLoader::resolve_global_config_text_without_environment(path, &text)
            .map_err(public_config_import_error)
    }

    pub(crate) fn stage_initial_setup_config_import(
        &mut self,
        config: ResolvedConfig,
    ) -> Result<(u64, Vec<InitialSetupConfigPublicValue>), String> {
        let generation = self.next_initial_setup_import_generation;
        self.next_initial_setup_import_generation = generation
            .checked_add(1)
            .ok_or_else(|| "initial setup import generation is exhausted".to_string())?;
        let values = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .map(|field| {
                let public = field.public_value(&config);
                InitialSetupConfigPublicValue {
                    key: field.label().to_string(),
                    text: public.value,
                    sensitive: public.sensitive,
                    configured: public.configured,
                }
            })
            .collect();
        self.pending_initial_setup_config_import = Some(PendingInitialSetupConfigImport {
            generation,
            workspace_root: self.app.workspace.authority_root().to_path_buf(),
            session_id: self.state.app_state.current_session_id,
            global_config_path: self.state.startup.global_config_path.clone(),
            setup_generation: self.state.startup.setup_generation,
            config_generation: self.state.provider_config.config_generation,
            config,
        });
        Ok((generation, values))
    }

    pub(super) fn reconcile_pending_initial_setup_config_import_owner(&mut self) {
        let owner_changed = self
            .pending_initial_setup_config_import
            .as_ref()
            .is_some_and(|candidate| {
                candidate.workspace_root != self.app.workspace.authority_root()
                    || candidate.session_id != self.state.app_state.current_session_id
                    || candidate.global_config_path != self.state.startup.global_config_path
                    || candidate.setup_generation != self.state.startup.setup_generation
                    || candidate.config_generation != self.state.provider_config.config_generation
            });
        if owner_changed {
            self.pending_initial_setup_config_import = None;
        }
    }

    pub(super) fn hydrate_initial_setup_import_sensitive_values(
        &self,
        mut values: Vec<(String, String)>,
        expected_import_generation: Option<u64>,
    ) -> Result<Vec<(String, String)>, String> {
        let Some(expected_generation) = expected_import_generation else {
            return Ok(values);
        };
        let candidate = self
            .pending_initial_setup_config_import
            .as_ref()
            .filter(|candidate| {
                candidate.generation == expected_generation
                    && candidate.workspace_root == self.app.workspace.authority_root()
                    && candidate.session_id == self.state.app_state.current_session_id
                    && candidate.global_config_path == self.state.startup.global_config_path
                    && candidate.setup_generation == self.state.startup.setup_generation
                    && candidate.config_generation == self.state.provider_config.config_generation
            })
            .ok_or_else(|| {
                "the imported configuration owner changed; import the TOML again".to_string()
            })?;
        let connection_values = values
            .iter()
            .filter_map(|(key, text)| {
                [ConfigField::BaseUrl, ConfigField::ProviderProfile]
                    .into_iter()
                    .find(|field| field.label() == key)
                    .map(|field| (field, text.as_str()))
            })
            .collect::<Vec<_>>();
        let final_config = apply_config_patch(
            self.state.global_config().clone(),
            crate::config::field::parse_config_field_patch(&connection_values)?,
        );
        let same_imported_connection =
            crate::config::ProviderEndpoint::parse(&final_config.model.base_url)
                .map_err(|error| error.to_string())?
                == crate::config::ProviderEndpoint::parse(&candidate.config.model.base_url)
                    .map_err(|error| error.to_string())?
                && final_config.model.provider_profile == candidate.config.model.provider_profile;
        for (key, text) in &mut values {
            let Some(field) = ConfigField::ALL
                .into_iter()
                .find(|field| field.label() == key.as_str())
            else {
                continue;
            };
            if field.is_sensitive()
                && text.trim().is_empty()
                && (field != ConfigField::ExtraHeadersJson || same_imported_connection)
            {
                *text = field.editor_value(&candidate.config);
            }
        }
        if same_imported_connection
            && !values
                .iter()
                .any(|(key, _)| key == ConfigField::ExtraHeadersJson.label())
        {
            values.push((
                ConfigField::ExtraHeadersJson.label().to_string(),
                ConfigField::ExtraHeadersJson.editor_value(&candidate.config),
            ));
        }
        Ok(values)
    }

    pub(crate) fn import_global_config_toml_path(&mut self, path: &Utf8Path) -> bool {
        match import_global_config_toml(path) {
            Ok(message) => {
                if !self.reload_config_with_status_code(DesktopStatusCode::ConfigImportFailed) {
                    return false;
                }
                self.state.set_status_message(message);
                true
            }
            Err(error) => {
                self.state.set_typed_status_message(
                    DesktopStatusCode::ConfigImportFailed,
                    format!("config import failed: {error}"),
                );
                false
            }
        }
    }
}
