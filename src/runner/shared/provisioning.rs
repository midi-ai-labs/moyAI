//! Hub requests select local templates; all filesystem authority stays in installed settings.
use super::*;

const LOCAL_FOLDER_TEMPLATE_ID: &str = "local-folder";

#[derive(Clone, Deserialize)]
pub(super) struct Environment {
    pub(super) id: String,
    pub(super) resource_id: String,
    pub(super) capacity: u32,
    #[serde(default)]
    pub(super) project_ids: Vec<String>,
    #[serde(default)]
    pub(super) enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProvisionDelivery {
    pub(super) environment_id: String,
    template_id: String,
    generation: u64,
    success: bool,
    error: Option<String>,
}
impl ProvisionDelivery {
    pub(crate) fn local_folder_environment(&self) -> bool {
        self.success && self.template_id == LOCAL_FOLDER_TEMPLATE_ID
    }
    pub(crate) fn desktop_environment(&self, environment_id: &str) -> bool {
        self.environment_id == environment_id
            && self.success
            && matches!(
                self.template_id.as_str(),
                crate::runner::provision::DESKTOP_TEMPLATE_ID | LOCAL_FOLDER_TEMPLATE_ID
            )
    }
}
#[derive(Deserialize)]
struct Request {
    environment_id: String,
    template_id: String,
    generation: u64,
    #[serde(default)]
    state: Option<String>,
}

impl Controller {
    async fn resource_catalog(&mut self) -> Result<Vec<Environment>, RunnerError> {
        let environments: Vec<Environment> = self
            .client
            .request("/v1/shared/runner/environments", None)
            .await?;
        if environments.len() > 128 {
            return Err(RunnerError::new(
                "Runner environment catalog exceeds its bound",
            ));
        }
        if matches!(self.settings.resource_scope, ResourceScope::Device) {
            let resource = environments.first().map(|env| env.resource_id.as_str());
            if environments
                .iter()
                .any(|env| env.capacity != 1 || Some(env.resource_id.as_str()) != resource)
            {
                return Err(RunnerError::new(
                    "Device resource mode requires every Hub environment on this Runner to use the same resource with capacity 1. Correct Hub setup or explicitly approve physical workspace isolation.",
                ));
            }
        }
        Ok(environments)
    }
    pub(super) async fn validate_resource_capacity(&mut self) -> Result<(), RunnerError> {
        self.validated_resource_catalog().await.map(|_| ())
    }
    pub(super) async fn validated_resource_catalog(
        &mut self,
    ) -> Result<Vec<Environment>, RunnerError> {
        let environments = self.resource_catalog().await?;
        self.retire_removed_local_folders(&environments)?;
        if self.settings.environments.iter().any(|mapping| {
            !environments
                .iter()
                .any(|env| env.id == mapping.environment_id)
        }) {
            return Err(RunnerError::new(
                "An installed environment is not registered to this Runner on the Hub",
            ));
        }
        Ok(environments)
    }
    fn retire_removed_local_folders(&mut self, catalog: &[Environment]) -> Result<(), RunnerError> {
        if !self.journal.active()?.is_empty()
            || !self.journal.retained_services()?.is_empty()
            || self
                .host
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?
                .runs
                .values()
                .any(|run| !run.processes_drained)
        {
            return Ok(());
        }
        let retired = {
            let store = self
                .host
                .inner
                .operations
                .lock()
                .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
            store
                .installed
                .provisions
                .iter()
                .filter(|receipt| receipt.local_folder_environment())
                .filter(|receipt| {
                    catalog
                        .iter()
                        .find(|env| env.id == receipt.environment_id)
                        .is_some_and(|env| !env.enabled && env.project_ids.is_empty())
                })
                .map(|receipt| receipt.environment_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        };
        if retired.is_empty() {
            return Ok(());
        }
        let mut settings = self.settings.clone();
        settings
            .environments
            .retain(|mapping| !retired.contains(&mapping.environment_id));
        settings.resolve()?;
        crate::runtime::resource_admission::register(&settings)?;
        {
            let mut store = self
                .host
                .inner
                .operations
                .lock()
                .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
            let mut installed = store.installed.clone();
            installed.settings = Some(settings.clone());
            installed
                .provisions
                .retain(|receipt| !retired.contains(&receipt.environment_id));
            store.update(installed)?;
        }
        self.settings = settings;
        Ok(())
    }
    fn unbind_missing_local_folders(
        &mut self,
        catalog: &[Environment],
    ) -> Result<Option<ProvisionDelivery>, RunnerError> {
        if !self.journal.active()?.is_empty()
            || !self.journal.retained_services()?.is_empty()
            || self
                .host
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?
                .runs
                .values()
                .any(|run| !run.processes_drained)
        {
            return Ok(None);
        }
        let receipts = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .provisions
            .iter()
            .filter(|receipt| receipt.local_folder_environment())
            .cloned()
            .collect::<Vec<_>>();
        let missing = receipts
            .iter()
            .filter(|receipt| {
                self.settings
                    .environments
                    .iter()
                    .find(|mapping| mapping.environment_id == receipt.environment_id)
                    .is_none_or(|mapping| {
                        std::fs::canonicalize(&mapping.directory)
                            .ok()
                            .and_then(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
                            .as_ref()
                            != Some(&mapping.directory)
                    })
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(None);
        }
        let missing_ids = missing
            .iter()
            .map(|receipt| receipt.environment_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut settings = self.settings.clone();
        settings
            .environments
            .retain(|mapping| !missing_ids.contains(mapping.environment_id.as_str()));
        if settings.environments.len() != self.settings.environments.len() {
            settings.resolve()?;
            crate::runtime::resource_admission::register(&settings)?;
            {
                let mut store = self
                    .host
                    .inner
                    .operations
                    .lock()
                    .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
                let mut installed = store.installed.clone();
                installed.settings = Some(settings.clone());
                store.update(installed)?;
            }
            self.settings = settings;
        }
        Ok(missing
            .into_iter()
            .find(|receipt| {
                catalog.iter().any(|env| {
                    env.id == receipt.environment_id && env.enabled && !env.project_ids.is_empty()
                })
            })
            .cloned())
    }

    async fn report_local_folder_missing(
        &mut self,
        receipt: &ProvisionDelivery,
    ) -> Result<(), RunnerError> {
        let report = ProvisionDelivery {
            environment_id: receipt.environment_id.clone(),
            template_id: LOCAL_FOLDER_TEMPLATE_ID.into(),
            generation: receipt.generation,
            success: false,
            error: Some("The selected project folder is missing or changed on this PC".into()),
        };
        let _: serde_json::Value = self
            .client
            .request(
                "/v1/shared/runner/provisioning",
                Some(
                    &serde_json::to_value(report)
                        .map_err(|error| RunnerError::new(error.to_string()))?,
                ),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn provision_environment(
        &mut self,
        template_id: &str,
        environment_id: &str,
    ) -> Result<(), RunnerError> {
        if !self.host.accepting_new_shared() {
            return Err(RunnerError::new("Provider is paused or in maintenance"));
        }
        if !self
            .resource_catalog()
            .await?
            .iter()
            .any(|env| env.id == environment_id)
        {
            return Err(RunnerError::new(
                "This environment is not assigned to this Runner by the Hub",
            ));
        }
        let pending: Vec<Request> = self
            .client
            .request("/v1/shared/runner/provisioning", None)
            .await?;
        if pending.iter().any(|request| {
            request.environment_id == environment_id
                && request.template_id == LOCAL_FOLDER_TEMPLATE_ID
        }) {
            return Err(RunnerError::new(
                "Choose this project's existing folder on this PC instead of creating a template folder",
            ));
        }
        let mut store = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
        let template = store
            .installed
            .templates
            .iter()
            .find(|template| template.id == template_id)
            .ok_or_else(|| {
                RunnerError::new("This template has not been approved by the local operator")
            })?;
        let mapping = super::super::provision::create(template, environment_id)?;
        let mut settings = self.settings.clone();
        if let Some(current) = settings
            .environments
            .iter()
            .find(|current| current.environment_id == environment_id)
        {
            if current != &mapping {
                return Err(RunnerError::new(
                    "This environment already has different local authority",
                ));
            }
        } else {
            if settings.environments.len() >= 128 {
                return Err(RunnerError::new("Runner environment capacity reached"));
            }
            settings.environments.push(mapping);
        }
        settings.resolve()?;
        crate::runtime::resource_admission::register(&settings)?;
        let mut installed = store.installed.clone();
        installed.settings = Some(settings.clone());
        store.update(installed)?;
        self.settings = settings;
        Ok(())
    }

    pub(super) async fn bind_project_folder(
        &mut self,
        project_id: &str,
        environment_id: &str,
        directory: camino::Utf8PathBuf,
        access_mode: crate::config::AccessMode,
        expected_directory: Option<camino::Utf8PathBuf>,
    ) -> Result<(), RunnerError> {
        if !crate::device_network::stable_id(project_id)
            || !crate::device_network::stable_id(environment_id)
            || !directory.is_absolute()
            || !directory.is_dir()
        {
            return Err(RunnerError::new(
                "Choose an existing absolute folder for this Hub project",
            ));
        }
        let directory = camino::Utf8PathBuf::from_path_buf(
            std::fs::canonicalize(&directory)
                .map_err(|_| RunnerError::new("Cannot open the chosen project folder"))?,
        )
        .map_err(|_| RunnerError::new("The chosen project folder is not UTF-8"))?;
        let catalog = self.resource_catalog().await?;
        let environment = catalog
            .iter()
            .find(|environment| environment.id == environment_id)
            .ok_or_else(|| RunnerError::new("This project is no longer assigned to this PC"))?;
        if environment.project_ids.as_slice() != [project_id] {
            return Err(RunnerError::new(
                "This execution place no longer belongs to the selected project",
            ));
        }
        let pending: Vec<Request> = self
            .client
            .request("/v1/shared/runner/provisioning", None)
            .await?;
        let pending = pending.iter().find(|request| {
            request.environment_id == environment_id
                && request.template_id == LOCAL_FOLDER_TEMPLATE_ID
        });
        let current = self
            .settings
            .environments
            .iter()
            .find(|mapping| mapping.environment_id == environment_id);
        let previously_selected = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .provisions
            .iter()
            .find(|receipt| {
                receipt.environment_id == environment_id && receipt.local_folder_environment()
            })
            .cloned();
        if current.is_none()
            && pending.is_none()
            && !(previously_selected.is_some() && environment.enabled)
        {
            return Err(RunnerError::new(
                "This project has no current local-folder setup request",
            ));
        }
        if current.is_some() && pending.is_none() && !environment.enabled {
            return Err(RunnerError::new(
                "This project is no longer enabled for execution on this PC",
            ));
        }
        if let Some(current) = current {
            if expected_directory.as_ref() != Some(&current.directory)
                && !(expected_directory.is_none()
                    && current.directory == directory
                    && current.access_mode == access_mode)
            {
                return Err(RunnerError::new(
                    "This project's folder changed; refresh its current setting",
                ));
            }
        } else if expected_directory.is_some() {
            return Err(RunnerError::new(
                "This project no longer has the previously selected folder",
            ));
        }
        // The controller processes this command between complete ticks. A local run,
        // unknown attempt, or retained process must settle before authority moves.
        if !self.journal.active()?.is_empty()
            || !self.journal.retained_services()?.is_empty()
            || self
                .host
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?
                .runs
                .values()
                .any(|run| !run.processes_drained)
        {
            return Err(RunnerError::new(
                "Stop this PC's current work and running apps before changing a project folder",
            ));
        }
        let mapping = EnvironmentMapping {
            environment_id: environment_id.into(),
            directory,
            access_mode,
            allowed_child_environments: Vec::new(),
        };
        let mut settings = self.settings.clone();
        if let Some(current) = settings
            .environments
            .iter_mut()
            .find(|current| current.environment_id == environment_id)
        {
            *current = mapping;
        } else {
            if settings.environments.len() >= 128 {
                return Err(RunnerError::new("Runner environment capacity reached"));
            }
            settings.environments.push(mapping);
        }
        settings.resolve()?;
        crate::runtime::resource_admission::register(&settings)?;
        {
            let mut store = self
                .host
                .inner
                .operations
                .lock()
                .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
            let mut installed = store.installed.clone();
            installed.settings = Some(settings.clone());
            installed
                .provisions
                .retain(|receipt| receipt.environment_id != environment_id);
            if installed.provisions.len() >= 128 {
                return Err(RunnerError::new(
                    "Runner environment receipt capacity reached",
                ));
            }
            installed.provisions.push(ProvisionDelivery {
                environment_id: environment_id.into(),
                template_id: LOCAL_FOLDER_TEMPLATE_ID.into(),
                generation: pending
                    .map(|request| request.generation)
                    .or_else(|| {
                        previously_selected
                            .as_ref()
                            .map(|receipt| receipt.generation)
                    })
                    .unwrap_or(0),
                success: true,
                error: None,
            });
            store.update(installed)?;
        }
        self.settings = settings;
        if let Some(pending) = pending {
            self.report_local_folder_ready(pending).await?;
        }
        Ok(())
    }

    async fn report_local_folder_ready(&mut self, request: &Request) -> Result<(), RunnerError> {
        let receipt = ProvisionDelivery {
            environment_id: request.environment_id.clone(),
            template_id: LOCAL_FOLDER_TEMPLATE_ID.into(),
            generation: request.generation,
            success: true,
            error: None,
        };
        let _: serde_json::Value = self
            .client
            .request(
                "/v1/shared/runner/provisioning",
                Some(
                    &serde_json::to_value(receipt)
                        .map_err(|error| RunnerError::new(error.to_string()))?,
                ),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn sync_provisioning(&mut self) -> Result<(), RunnerError> {
        let templates = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .templates
            .iter()
            .map(|template| json!({"id":template.id,"label":template.label}))
            .collect::<Vec<_>>();
        let _: serde_json::Value = self
            .client
            .request(
                "/v1/shared/runner/templates",
                Some(&json!({"templates":templates})),
            )
            .await?;
        let has_local_folders = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .provisions
            .iter()
            .any(ProvisionDelivery::local_folder_environment);
        if has_local_folders {
            let catalog = self.resource_catalog().await?;
            self.retire_removed_local_folders(&catalog)?;
            if let Some(receipt) = self.unbind_missing_local_folders(&catalog)? {
                self.report_local_folder_missing(&receipt).await?;
            }
        }
        let pending: Vec<Request> = self
            .client
            .request("/v1/shared/runner/provisioning", None)
            .await?;
        if pending.len() > 128 {
            return Err(RunnerError::new("Provision request bound exceeded"));
        }
        for request in pending.iter().filter(|request| {
            request.template_id == LOCAL_FOLDER_TEMPLATE_ID
                && request.state.as_deref().unwrap_or("pending") == "pending"
        }) {
            if self
                .settings
                .environments
                .iter()
                .any(|mapping| mapping.environment_id == request.environment_id)
            {
                self.report_local_folder_ready(request).await?;
            }
        }
        let Some(request) = pending
            .into_iter()
            .find(|request| request.template_id != LOCAL_FOLDER_TEMPLATE_ID)
        else {
            return Ok(());
        };
        let existing = self
            .host
            .inner
            .operations
            .lock()
            .map_err(|_| RunnerError::new("Runner operations unavailable"))?
            .installed
            .provisions
            .iter()
            .find(|value| {
                value.environment_id == request.environment_id
                    && value.generation == request.generation
            })
            .cloned();
        let delivery = if let Some(existing) = existing {
            if existing.template_id != request.template_id {
                return Err(RunnerError::new("Provision request identity changed"));
            }
            existing
        } else {
            let result = self
                .provision_environment(&request.template_id, &request.environment_id)
                .await;
            let delivery = ProvisionDelivery {
                environment_id: request.environment_id,
                template_id: request.template_id,
                generation: request.generation,
                success: result.is_ok(),
                error: result.err().map(|error| error.message),
            };
            let mut store = self
                .host
                .inner
                .operations
                .lock()
                .map_err(|_| RunnerError::new("Runner operations unavailable"))?;
            let mut next = store.installed.clone();
            next.provisions
                .retain(|value| value.environment_id != delivery.environment_id);
            if next.provisions.len() >= 128 {
                return Err(RunnerError::new("Provision receipt capacity reached"));
            }
            next.provisions.push(delivery.clone());
            store.update(next)?;
            delivery
        };
        let _: serde_json::Value = self
            .client
            .request(
                "/v1/shared/runner/provisioning",
                Some(&serde_json::to_value(delivery).map_err(|e| RunnerError::new(e.to_string()))?),
            )
            .await?;
        Ok(())
    }
}
