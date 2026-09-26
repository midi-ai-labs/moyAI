//! Jobs started from an ordinary local chat are read back through the same
//! device certificate and current Hub grant, without copying local history.
use super::*;
use std::collections::BTreeSet;

const MAX_ORIGIN_PAGES: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginJob {
    pub project_id: String,
    pub job: WorkSummary,
    #[serde(default)]
    pub artifacts: Vec<WorkAsset>,
    #[serde(default)]
    pub more_artifacts: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginRetainedService {
    pub project_id: String,
    pub service: OriginServiceView,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginServiceView {
    pub service_id: String,
    pub conversation_id: String,
    pub environment_id: String,
    pub expires_at_ms: u64,
    pub stop_requested: bool,
    pub uncertain: bool,
    #[serde(default)]
    pub can_stop: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OriginWorkProjection {
    pub origin_session_ref: String,
    pub jobs: Vec<OriginJob>,
    pub retained_services: Vec<OriginRetainedService>,
    #[serde(default)]
    pub hidden_active_work: bool,
    pub observed_at_ms: u64,
    pub admission_revision: String,
    #[serde(default)]
    pub stop_pending: bool,
    #[serde(default)]
    pub stop_error: Option<String>,
}

#[derive(Deserialize)]
struct OriginWorkPage {
    origin_session_ref: String,
    jobs: Vec<OriginJob>,
    retained_services: Vec<OriginRetainedService>,
    #[serde(default)]
    hidden_active_work: bool,
    next_job_before: Option<String>,
    next_service_before: Option<String>,
}

fn append_origin_page(
    projection: &mut OriginWorkProjection,
    page: OriginWorkPage,
    seen_jobs: &mut BTreeSet<String>,
    seen_services: &mut BTreeSet<String>,
) -> Result<(Option<String>, Option<String>), String> {
    if page.origin_session_ref != projection.origin_session_ref
        || page
            .next_job_before
            .as_deref()
            .is_some_and(|id| !super::super::stable_id(id))
        || page
            .next_service_before
            .as_deref()
            .is_some_and(|id| !super::super::stable_id(id))
    {
        return Err("Hub returned a different or invalid chat origin".into());
    }
    projection.hidden_active_work |= page.hidden_active_work;
    for row in page.jobs {
        if !super::super::stable_id(&row.project_id)
            || !super::super::stable_id(&row.job.id)
            || row.job.origin_session_ref.as_deref() != Some(&projection.origin_session_ref)
            || row.artifacts.len() > 20
            || row.artifacts.iter().any(|asset| {
                asset.project_id != row.project_id
                    || asset.job_id.as_deref() != Some(&row.job.id)
                    || asset.kind != "artifact"
                    || asset.purged_at_ms.is_some()
                    || !super::super::stable_id(&asset.id)
            })
            || !seen_jobs.insert(row.job.id.clone())
        {
            return Err("Hub returned an inconsistent chat job page".into());
        }
        projection.jobs.push(row);
    }
    for row in page.retained_services {
        if ![
            row.project_id.as_str(),
            row.service.service_id.as_str(),
            row.service.conversation_id.as_str(),
            row.service.environment_id.as_str(),
        ]
        .into_iter()
        .all(super::super::stable_id)
            || !seen_services.insert(row.service.service_id.clone())
        {
            return Err("Hub returned an inconsistent chat app page".into());
        }
        projection.retained_services.push(row);
    }
    Ok((page.next_job_before, page.next_service_before))
}

fn origin_cursors_advance(
    previous_jobs: Option<&str>,
    previous_services: Option<&str>,
    next_jobs: Option<&str>,
    next_services: Option<&str>,
) -> bool {
    !next_jobs.is_some_and(|next| Some(next) == previous_jobs)
        && !next_services.is_some_and(|next| Some(next) == previous_services)
}

impl DeviceNetworkService {
    pub(crate) async fn origin_revision(&self, origin_session_ref: &str) -> Result<u64, String> {
        let session_id = origin_session_ref
            .parse::<crate::session::SessionId>()
            .map_err(|_| "Invalid local chat identity".to_string())?;
        if session_id.to_string() != origin_session_ref {
            return Err("Invalid local chat identity".into());
        }
        let expectation = self
            .inner
            .store
            .session_repo()
            .active_turn_expectation_for_session(session_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "The local chat no longer exists".to_string())?;
        Ok(match expectation {
            crate::session::ActiveTurnExpectation::Idle { revision, .. }
            | crate::session::ActiveTurnExpectation::Turn { revision, .. } => revision,
        })
    }

    pub(crate) async fn origin_work(
        &self,
        origin_session_ref: &str,
    ) -> Result<OriginWorkProjection, String> {
        if !super::super::stable_id(origin_session_ref) {
            return Err("Invalid local chat identity".into());
        }
        let admission_revision = self.origin_revision(origin_session_ref).await?;
        let (connection, session) = self.agent_session().await?;
        let stop_error = self
            .retry_origin_turn_stops_with_session(&connection, &session)
            .await
            .err();
        let stop_pending = self
            .inner
            .shared_work
            .0
            .lock()
            .unwrap()
            .receipts
            .pending_origin_stops(&connection.hub_binding)
            .iter()
            .any(|receipt| {
                matches!(&receipt.operation,
                ReceiptOperation::StopOriginTurn { origin_session_ref: pending, .. }
                | ReceiptOperation::StopOriginTurnPrepared { origin_session_ref: pending, .. }
                | ReceiptOperation::StopOriginConversation { origin_session_ref: pending, .. }
                    if pending == origin_session_ref)
            });
        let mut projection = OriginWorkProjection {
            origin_session_ref: origin_session_ref.to_owned(),
            jobs: Vec::new(),
            retained_services: Vec::new(),
            hidden_active_work: false,
            observed_at_ms: now_ms(),
            admission_revision: admission_revision.to_string(),
            stop_pending,
            stop_error: stop_pending.then_some(stop_error).flatten(),
        };
        let mut seen_jobs = BTreeSet::new();
        let mut seen_services = BTreeSet::new();
        let mut job_before: Option<String> = None;
        let mut service_before: Option<String> = None;
        let mut include_jobs = true;
        let mut include_services = true;
        for _ in 0..MAX_ORIGIN_PAGES {
            let mut query = Vec::new();
            if !include_jobs {
                query.push(("include_jobs", "false"));
            }
            if !include_services {
                query.push(("include_services", "false"));
            }
            if let Some(before) = job_before.as_deref() {
                query.push(("job_before", before));
            }
            if let Some(before) = service_before.as_deref() {
                query.push(("service_before", before));
            }
            let page: OriginWorkPage = request(
                &connection.client,
                &format!("origins/{origin_session_ref}/work"),
                Some(&session.token),
                None,
                &query,
            )
            .await
            .map_err(|error| error.message().to_string())?;
            let (next_jobs, next_services) =
                append_origin_page(&mut projection, page, &mut seen_jobs, &mut seen_services)?;
            if (!include_jobs || next_jobs.is_none())
                && (!include_services || next_services.is_none())
            {
                if self
                    .shared_connection()
                    .is_none_or(|current| current.binding != connection.binding)
                {
                    return Err("Hub connection changed while reading this chat".into());
                }
                projection.observed_at_ms = now_ms();
                return Ok(projection);
            }
            if !origin_cursors_advance(
                job_before.as_deref(),
                service_before.as_deref(),
                next_jobs.as_deref(),
                next_services.as_deref(),
            ) {
                return Err("Hub did not advance the chat work page".into());
            }
            include_jobs = next_jobs.is_some();
            include_services = next_services.is_some();
            job_before = next_jobs;
            service_before = next_services;
        }
        Err("The chat has too many Hub pages to show safely; no stop request was sent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_page_rejects_wrong_origin_before_display() {
        let mut projection = OriginWorkProjection {
            origin_session_ref: "local-session-a".into(),
            jobs: vec![],
            retained_services: vec![],
            hidden_active_work: false,
            observed_at_ms: 0,
            admission_revision: "0".into(),
            stop_pending: false,
            stop_error: None,
        };
        let page = OriginWorkPage {
            origin_session_ref: "local-session-b".into(),
            jobs: vec![],
            retained_services: vec![],
            hidden_active_work: true,
            next_job_before: None,
            next_service_before: None,
        };
        assert!(
            append_origin_page(
                &mut projection,
                page,
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            )
            .is_err()
        );
        assert!(projection.jobs.is_empty());
        assert!(!projection.hidden_active_work);
    }

    #[test]
    fn hidden_active_work_is_projected_without_disclosing_job_details() {
        let mut projection = OriginWorkProjection {
            origin_session_ref: "local-session-a".into(),
            jobs: vec![],
            retained_services: vec![],
            hidden_active_work: false,
            observed_at_ms: 0,
            admission_revision: "0".into(),
            stop_pending: false,
            stop_error: None,
        };
        let page = OriginWorkPage {
            origin_session_ref: "local-session-a".into(),
            jobs: vec![],
            retained_services: vec![],
            hidden_active_work: true,
            next_job_before: None,
            next_service_before: None,
        };
        append_origin_page(
            &mut projection,
            page,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )
        .expect("same origin");
        assert!(projection.hidden_active_work);
        assert!(projection.jobs.is_empty());
        assert!(projection.retained_services.is_empty());
    }

    #[test]
    fn exhausted_job_stream_does_not_block_service_pages() {
        assert!(origin_cursors_advance(
            None,
            None,
            None,
            Some("service-128")
        ));
        assert!(origin_cursors_advance(
            None,
            Some("service-128"),
            None,
            Some("service-256")
        ));
        assert!(!origin_cursors_advance(
            None,
            Some("service-128"),
            None,
            Some("service-128")
        ));
    }
}
