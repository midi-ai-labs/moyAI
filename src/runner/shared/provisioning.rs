//! Hub requests select local templates; all filesystem authority stays in installed settings.
use super::*;

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
    environment_id: String,
    template_id: String,
    generation: u64,
    success: bool,
    error: Option<String>,
}
impl ProvisionDelivery {
    pub(crate) fn desktop_environment(&self, environment_id: &str) -> bool {
        self.environment_id == environment_id
            && self.success
            && self.template_id == crate::runner::provision::DESKTOP_TEMPLATE_ID
    }
}
#[derive(Deserialize)]
struct Request {
    environment_id: String,
    template_id: String,
    generation: u64,
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
        let pending: Vec<Request> = self
            .client
            .request("/v1/shared/runner/provisioning", None)
            .await?;
        if pending.len() > 128 {
            return Err(RunnerError::new("Provision request bound exceeded"));
        }
        let Some(request) = pending.into_iter().next() else {
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
