use super::*;

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
    #[serde(default)]
    pub approval_status: Option<String>,
    #[serde(default)]
    pub approval_decision: Option<String>,
    #[serde(default)]
    pub approval_context: Option<WorkApprovalContext>,
}
impl DeviceNetworkService {
    /// Background attention polling does not change selection, drafts, query
    /// generation, or cached inbox pages during normal polling. It renews the
    /// same device credential owner as foreground requests when necessary.
    /// Only a fresh authenticated reply may
    /// produce a native notification; disconnected or replaced identities are
    /// discarded instead of projecting the last known actionable approval.
    pub async fn shared_notification_inbox(&self) -> Option<(String, WorkInbox)> {
        let connection = self.shared_connection()?;
        if self.inner.state.lock().ok()?.status != "active" {
            return None;
        }
        let (generation, query) = {
            let mut runtime = self.inner.shared_work.0.lock().ok()?;
            if runtime.binding != connection.binding
                && (runtime.token.is_some() || !runtime.view.projects.is_empty())
            {
                // Replacing the device/Hub identity retires every old target,
                // exactly as the foreground projection does.
                runtime.clear();
            }
            (runtime.generation, runtime.query)
        };
        let token = self
            .shared_authenticate(&connection, generation, query)
            .await
            .ok()?;
        let principal = {
            let runtime = self.inner.shared_work.0.lock().ok()?;
            if runtime.generation != generation || runtime.binding != connection.binding {
                return None;
            }
            runtime.view.principal.as_ref()?.user_id.clone()
        };
        let inbox: WorkInbox = match request(
            &connection.client,
            "inbox",
            Some(&token),
            None,
            &[("limit", "100")],
        )
        .await
        {
            Ok(inbox) => inbox,
            Err(RequestError::Http(401 | 403)) => {
                let mut runtime = self.inner.shared_work.0.lock().ok()?;
                if runtime.generation == generation
                    && runtime.binding == connection.binding
                    && runtime.token.as_deref() == Some(&token)
                {
                    runtime.clear();
                }
                return None;
            }
            Err(_) => return None,
        };
        if inbox.items.len() > 100
            || inbox.items.iter().any(|item| {
                !super::super::stable_id(&item.job_id)
                    || !super::super::stable_id(&item.project_id)
                    || item
                        .approval_id
                        .as_deref()
                        .is_some_and(|id| !super::super::stable_id(id))
                    || item.approval_context.as_ref().is_some_and(|context| {
                        context.job_id != item.job_id || context.project_id != item.project_id
                    })
            })
            || self
                .shared_connection()
                .is_none_or(|current| current.binding != connection.binding)
            || self.inner.state.lock().ok()?.status != "active"
        {
            return None;
        }
        let runtime = self.inner.shared_work.0.lock().ok()?;
        if runtime.generation != generation
            || runtime.binding != connection.binding
            || runtime.token.as_deref() != Some(&token)
            || runtime
                .view
                .principal
                .as_ref()
                .map(|person| person.user_id.as_str())
                != Some(&principal)
            || runtime
                .view
                .expires_at_ms
                .is_none_or(|until| until <= now_ms())
        {
            return None;
        }
        Some((
            format!("{}|{generation}|{principal}", connection.binding),
            inbox,
        ))
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
        // An inbox item targets an exact job, which may be a child of the
        // currently selected conversation or belong to another project.
        runtime.view.selected_conversation_id = None;
        runtime.view.selected_job_id = Some(item.job_id);
        runtime.view.detail = None;
        runtime.view.approval = None;
        runtime.view.assets.clear();
        runtime.view.transcript = None;
        runtime.view.conversation_history = None;
        runtime.transcript_after = 0;
        runtime.history_before = None;
        runtime.before = None;
        runtime.environment_before = None;
        Ok(())
    }
}
