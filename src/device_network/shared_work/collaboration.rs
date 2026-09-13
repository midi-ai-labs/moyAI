use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkHandover {
    pub candidates: Vec<WorkPerson>,
    pub pending: Option<PendingHandover>,
    pub can_handover: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingHandover {
    pub new_assignee_id: String,
    pub requested_by: String,
    pub requested_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkInbox {
    pub items: Vec<WorkNotification>,
    pub next_before: Option<String>,
    pub unread_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkNotification {
    pub id: String,
    pub job_id: String,
    pub project_id: String,
    pub kind: String,
    pub title: String,
    pub created_at_ms: u64,
    pub read_at_ms: Option<u64>,
    pub can_act: bool,
    pub approval_id: Option<String>,
}
impl DeviceNetworkService {
    pub(super) async fn shared_handover(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        project_id: &str,
        job_id: &str,
        expected_revision: u64,
        new_assignee_id: &str,
    ) -> Result<(), RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_project(project_id, generation, query)?;
            if !runtime
                .view
                .detail
                .as_ref()
                .is_some_and(|j| j.id == job_id && j.revision == expected_revision)
                || !runtime.view.handover.as_ref().is_some_and(|v| {
                    v.can_handover && v.candidates.iter().any(|p| p.user_id == new_assignee_id)
                })
            {
                return Err(RequestError::Local(
                    "最新の担当者候補と仕事を確認してください。",
                ));
            }
        }
        request::<WorkDetail>(
            &connection.client,
            &format!("jobs/{job_id}/handover"),
            Some(token),
            Some(json!({"expected_revision":expected_revision,"new_assignee_id":new_assignee_id})),
            &[],
        )
        .await?;
        Ok(())
    }
    pub(super) async fn shared_inbox_open(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        notification_id: &str,
    ) -> Result<(), RequestError> {
        let item = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_current(generation, query)?;
            runtime
                .view
                .inbox
                .as_ref()
                .and_then(|v| v.items.iter().find(|item| item.id == notification_id))
                .cloned()
                .ok_or(RequestError::Invalid)?
        };
        let mut url = reqwest::Url::parse("https://local/").map_err(|_| RequestError::Invalid)?;
        url.path_segments_mut()
            .map_err(|_| RequestError::Invalid)?
            .push(&item.id);
        request::<Value>(
            &connection.client,
            &format!("inbox/{}/read", url.path().trim_start_matches('/')),
            Some(token),
            Some(json!({})),
            &[],
        )
        .await?;
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.require_current(generation, query)?;
        if !runtime
            .view
            .projects
            .iter()
            .any(|p| p.id == item.project_id)
        {
            return Err(RequestError::Http(403));
        }
        if runtime.view.selected_project_id.as_deref() != Some(&item.project_id) {
            runtime.view.inputs.clear();
        }
        runtime.view.selected_project_id = Some(item.project_id);
        runtime.view.selected_job_id = Some(item.job_id);
        runtime.view.detail = None;
        runtime.view.approval = None;
        runtime.view.handover = None;
        runtime.view.assets.clear();
        runtime.view.transcript = None;
        runtime.transcript_after = 0;
        runtime.before = None;
        runtime.environment_before = None;
        Ok(())
    }
}
