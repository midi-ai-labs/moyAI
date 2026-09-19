use super::*;
use crate::runner::{RunnerCommand, RunnerResponse, operations::RunnerOperation};

impl Runtime {
    fn provider_template(
        &self,
        binding: &str,
        generation: u64,
    ) -> Result<crate::runner::provision::ProvisionTemplate, RequestError> {
        if self.generation != generation {
            return Err(RequestError::Invalid);
        }
        if self.provider_binding.is_empty() || self.provider_binding != binding {
            return Err(RequestError::Local(
                "Hubまたは端末の接続設定が変わりました。親フォルダを選び直してください。",
            ));
        }
        self.view.provider_draft.clone().ok_or(RequestError::Local(
            "作成を許可する親フォルダを先に選択してください。",
        ))
    }
}

impl DeviceNetworkService {
    pub(super) async fn shared_provider_command(
        &self,
        expected_generation: &str,
        command: SharedWorkCommand,
    ) -> SharedWorkProjection {
        let generation = {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.generation.to_string() != expected_generation {
                drop(runtime);
                return self.shared_work_projection();
            }
            runtime.view.provider_error = None;
            runtime.generation
        };
        let result = self.shared_provider_execute(generation, command).await;
        {
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.generation == generation {
                if let Err(error) = result {
                    runtime.view.provider_error = Some(error.message().into());
                }
            }
        }
        self.shared_work_projection()
    }
    async fn shared_provider_execute(
        &self,
        generation: u64,
        command: SharedWorkCommand,
    ) -> Result<(), RequestError> {
        if let SharedWorkCommand::ProviderPrepare {
            id,
            label,
            access_mode,
            allowed_child_environments,
            resource_scope,
        } = command
        {
            if !super::super::stable_id(&id)
                || label.trim().is_empty()
                || label.len() > 256
                || allowed_child_environments.len() > 128
                || allowed_child_environments
                    .iter()
                    .any(|id| !super::super::stable_id(id))
            {
                return Err(RequestError::Invalid);
            }
            let connection = self.shared_connection().ok_or(RequestError::Unavailable)?;
            let Some(base_root) = choose_root().await? else {
                return Ok(());
            };
            if self
                .shared_connection()
                .is_none_or(|current| current.binding != connection.binding)
            {
                return Err(RequestError::Local(
                    "Hubまたは端末の接続設定が変わりました。親フォルダを選び直してください。",
                ));
            }
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            if runtime.generation != generation {
                return Ok(());
            }
            runtime.view.provider_draft = Some(crate::runner::provision::ProvisionTemplate {
                id,
                label,
                access_mode,
                base_root,
                allowed_child_environments,
            });
            runtime.view.provider_scope = resource_scope;
            runtime.provider_binding = connection.binding;
            return Ok(());
        }
        let operation = match command {
            SharedWorkCommand::ProviderRemoveTemplate {
                runner_id,
                template_id,
            } => {
                let runtime = self.inner.shared_work.0.lock().unwrap();
                let provider = runtime
                    .view
                    .provider
                    .as_ref()
                    .ok_or(RequestError::Invalid)?;
                if runtime.generation != generation
                    || provider.runner_id != runner_id
                    || !provider
                        .templates
                        .iter()
                        .any(|template| template.id == template_id)
                {
                    return Err(RequestError::Invalid);
                }
                let expected_templates = provider.templates.clone();
                let templates = expected_templates
                    .iter()
                    .filter(|template| template.id != template_id)
                    .cloned()
                    .collect();
                Some((
                    runner_id,
                    RunnerOperation::UpdateTemplates {
                        templates,
                        expected_templates,
                    },
                ))
            }
            SharedWorkCommand::ProviderOperation {
                runner_id,
                operation,
            } => {
                if matches!(
                    operation,
                    RunnerOperation::InstallSettings { .. }
                        | RunnerOperation::InstallDesktop { .. }
                        | RunnerOperation::UpdateTemplates { .. }
                ) {
                    return Err(RequestError::Local(
                        "提供設定はフォルダを選択して確認してから適用してください。",
                    ));
                }
                Some((runner_id, operation))
            }
            SharedWorkCommand::ProviderInstall { runner_id } => {
                let connection = self.shared_connection().ok_or(RequestError::Unavailable)?;
                let hub_id = connection.hub_id.clone();
                let runtime = self.inner.shared_work.0.lock().unwrap();
                if runtime.generation != generation {
                    return Ok(());
                }
                let template = runtime.provider_template(&connection.binding, generation)?;
                if let Some(provider) = &runtime.view.provider {
                    if provider.runner_id != runner_id {
                        return Err(RequestError::Invalid);
                    }
                    if provider.mode == "shared" {
                        let mut templates = provider.templates.clone();
                        templates.retain(|t| t.id != template.id);
                        templates.push(template);
                        Some((
                            runner_id,
                            RunnerOperation::UpdateTemplates {
                                templates,
                                expected_templates: provider.templates.clone(),
                            },
                        ))
                    } else {
                        Some((
                            runner_id,
                            RunnerOperation::InstallSettings {
                                settings: crate::runner::shared::SharedSettings {
                                    version: 1,
                                    hub_id,
                                    device_id: connection.client.device_id.clone(),
                                    environments: Vec::new(),
                                    resource_scope: runtime.view.provider_scope.clone(),
                                },
                                templates: vec![template],
                            },
                        ))
                    }
                } else {
                    return Err(RequestError::Local("提供状態を先に取得してください。"));
                }
            }
            SharedWorkCommand::ProviderStart => {
                tokio::task::spawn_blocking(crate::runner::operations::launch)
                    .await
                    .map_err(|_| RequestError::Unavailable)?
                    .map_err(|e| RequestError::Filesystem(e.to_string()))?;
                self.inner.shared_work.0.lock().unwrap().view.feedback =
                    Some("Runnerを起動しました。数秒後に提供状態を更新してください。".into());
                return Ok(());
            }
            _ => None,
        };
        let result = tokio::task::spawn_blocking(move || {
            let (runner_id, operation) = match operation {
                Some(value) => value,
                None => match local_request(&RunnerCommand::Identity)? {
                    RunnerResponse::Identity { identity } => {
                        (identity.runner_id, RunnerOperation::Status)
                    }
                    _ => return Err(RequestError::Invalid),
                },
            };
            local_request(&RunnerCommand::Operations {
                runner_id,
                operation,
            })
        })
        .await
        .map_err(|_| RequestError::Unavailable)??;
        let RunnerResponse::Operations { projection } = result else {
            return Err(RequestError::Invalid);
        };
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        if runtime.generation == generation {
            runtime.view.provider = Some(projection);
        }
        Ok(())
    }
}
#[cfg(windows)]
fn local_request(command: &RunnerCommand) -> Result<RunnerResponse, RequestError> {
    crate::runner::windows::request(command).map_err(|e| RequestError::Filesystem(e.to_string()))
}
#[cfg(not(windows))]
fn local_request(_: &RunnerCommand) -> Result<RunnerResponse, RequestError> {
    Err(RequestError::Local(
        "端末の提供操作はWindowsで実行してください。",
    ))
}
#[cfg(feature = "tauri-desktop")]
async fn choose_root() -> Result<Option<camino::Utf8PathBuf>, RequestError> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("実行領域の作成を許可する親フォルダ")
            .pick_folder()
            .map(camino::Utf8PathBuf::from_path_buf)
            .transpose()
            .map_err(|_| RequestError::Invalid)
    })
    .await
    .map_err(|_| RequestError::Unavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_folder_consent_does_not_follow_replaced_hub_trust() {
        let runtime = Runtime {
            generation: 1,
            provider_binding: "hub-a|device-a|url|ca-a".into(),
            view: SharedWorkProjection {
                provider_draft: Some(crate::runner::provision::ProvisionTemplate {
                    id: "template-a".into(),
                    label: "Local execution root".into(),
                    base_root: "C:/fixture".into(),
                    access_mode: crate::config::AccessMode::Default,
                    allowed_child_environments: Vec::new(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            runtime
                .provider_template("hub-a|device-a|url|ca-a", 1)
                .is_ok()
        );
        assert!(
            runtime
                .provider_template("hub-a|device-a|url|ca-b", 1)
                .is_err()
        );
        assert!(
            runtime
                .provider_template("hub-b|device-b|url|ca-b", 1)
                .is_err()
        );
    }
}
#[cfg(not(feature = "tauri-desktop"))]
async fn choose_root() -> Result<Option<camino::Utf8PathBuf>, RequestError> {
    Err(RequestError::Local("フォルダはDesktopで選択してください。"))
}
