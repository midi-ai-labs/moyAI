//! Public catalog facts captured with an explicit, durable Main/Side review.
//! This is a comparison baseline, never an allocation or a second review gate.

use serde::{Deserialize, Serialize};

use super::{CatalogRevision, HubCatalog, HubError, HubModel, ReviewedHubSelection};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubCatalogBaseline {
    pub hub_id: String,
    pub revision: CatalogRevision,
    pub software_version: String,
    pub models: Vec<HubModel>,
}

impl HubCatalogBaseline {
    pub(super) fn capture(catalog: &HubCatalog) -> Self {
        Self {
            hub_id: catalog.hub_id.clone(),
            revision: catalog.revision,
            software_version: catalog.software_version.clone(),
            models: catalog.models.clone(),
        }
    }

    fn catalog(&self) -> HubCatalog {
        HubCatalog {
            hub_id: self.hub_id.clone(),
            revision: self.revision,
            software_version: self.software_version.clone(),
            models: self.models.clone(),
            changes: Vec::new(),
        }
    }

    pub(super) fn validate(&self, review: &ReviewedHubSelection) -> Result<(), HubError> {
        review.check_admission(&self.catalog())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HubCatalogComparisonStatus {
    FirstReview,
    BaselineUnavailable,
    CurrentUnavailable,
    Compared,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubCatalogModelChange {
    pub id: String,
    pub before: Option<HubModel>,
    pub after: Option<HubModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubCatalogComparison {
    pub status: HubCatalogComparisonStatus,
    pub reviewed_revision: Option<CatalogRevision>,
    pub current_revision: Option<CatalogRevision>,
    pub software_before: Option<String>,
    pub software_after: Option<String>,
    pub models: Vec<HubCatalogModelChange>,
}

impl HubCatalogComparison {
    pub(super) fn between(
        review: Option<&ReviewedHubSelection>,
        baseline: Option<&HubCatalogBaseline>,
        catalog: Option<&HubCatalog>,
    ) -> Self {
        use HubCatalogComparisonStatus as Status;
        let mut result = Self {
            status: Status::CurrentUnavailable,
            reviewed_revision: review.map(|review| review.reviewed_revision),
            current_revision: catalog.map(|catalog| catalog.revision),
            software_before: baseline.map(|baseline| baseline.software_version.clone()),
            software_after: catalog.map(|catalog| catalog.software_version.clone()),
            models: Vec::new(),
        };
        let Some(catalog) = catalog else {
            return result;
        };
        let Some(review) = review else {
            result.status = if baseline.is_some() {
                Status::Invalid
            } else {
                Status::FirstReview
            };
            return result;
        };
        let Some(baseline) = baseline else {
            result.status = Status::BaselineUnavailable;
            return result;
        };
        // Journal entries are intentionally absent from the baseline. Compare the same public
        // model/software facts on both sides, retaining revision/identity validation.
        let current = HubCatalogBaseline::capture(catalog).catalog();
        if baseline.validate(review).is_err()
            || catalog.validate().is_err()
            || current.diff(&baseline.catalog()).is_err()
        {
            result.status = Status::Invalid;
            return result;
        }
        result.status = Status::Compared;
        for model in &catalog.models {
            let before = baseline.models.iter().find(|old| old.id == model.id);
            if before != Some(model) {
                result.models.push(HubCatalogModelChange {
                    id: model.id.clone(),
                    before: before.cloned(),
                    after: Some(model.clone()),
                });
            }
        }
        for model in &baseline.models {
            if !catalog.models.iter().any(|current| current.id == model.id) {
                result.models.push(HubCatalogModelChange {
                    id: model.id.clone(),
                    before: Some(model.clone()),
                    after: None,
                });
            }
        }
        result.models.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }
}

#[cfg(test)]
mod tests;
