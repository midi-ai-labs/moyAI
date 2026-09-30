//! Native attention is a delivery projection, never a second approval authority.
use std::collections::HashSet;

use crate::device_network::WorkInbox;

pub(super) struct SharedAttention {
    identity: String,
    delivered: HashSet<String>,
    started_at_ms: u64,
}

impl Default for SharedAttention {
    fn default() -> Self {
        Self {
            identity: String::new(),
            delivered: HashSet::new(),
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

impl SharedAttention {
    pub fn observe(&mut self, identity: &str, inbox: &WorkInbox) -> Vec<String> {
        if self.identity != identity {
            self.identity = identity.into();
            self.delivered.clear();
        }
        // Keep bounded delivery memory for the current authenticated inbox page.
        // Unavailable polls do not call observe and cannot replay cached authority.
        self.delivered
            .retain(|id| inbox.items.iter().any(|item| &item.id == id));
        let mut messages = Vec::new();
        for item in &inbox.items {
            // An ordinary model question arrives as a final response, not a
            // typed permission request. Notify the response without guessing
            // intent from punctuation or claiming that the user's goal is done.
            if item.kind == "finished"
                && item.read_at_ms.is_none()
                && item.created_at_ms >= self.started_at_ms
                && !self.delivered.contains(&item.id)
            {
                messages.push(format!(
                    "{}：実行が終了しました。返答や質問を会話で確認してください。",
                    item.title.chars().take(48).collect::<String>()
                ));
                self.delivered.insert(item.id.clone());
                continue;
            }
            if item.kind != "approval"
                || !item.can_act
                || item.approval_status.as_deref() != Some("pending")
                || item.approval_id.is_none()
                || self.delivered.contains(&item.id)
            {
                continue;
            }
            let title: String = item.title.chars().take(48).collect();
            let pc = item
                .approval_context
                .as_ref()
                .map(|context| {
                    context
                        .execution_device_label
                        .as_deref()
                        .unwrap_or(&context.execution_device_id)
                })
                .unwrap_or("実行PC");
            messages.push(format!(
                "{title}：{pc}で行う操作の承認を待っています。moyAIの会話で内容を確認してください。"
            ));
            self.delivered.insert(item.id.clone());
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inbox(can_act: bool, status: &str) -> WorkInbox {
        serde_json::from_value(json!({"items":[{
            "id":"notice-1","job_id":"child-1","project_id":"project-1",
            "kind":"approval","title":"時計","created_at_ms":1,"read_at_ms":null,
            "can_act":can_act,"approval_id":"approval-1","approval_status":status,
            "approval_context":{"job_id":"child-1","project_id":"project-1","root_id":"root-1",
                "conversation_id":"root-1","job_title":"時計","controller_device_id":"a",
                "controller_device_label":"WinA","execution_device_id":"b","execution_device_label":"WinB"}
        }],"next_before":null,"unread_count":1})).unwrap()
    }

    #[test]
    fn only_fresh_actionable_pending_approval_notifies_once_per_identity() {
        let mut delivery = SharedAttention::default();
        assert!(
            delivery
                .observe("WinB", &inbox(false, "pending"))
                .is_empty()
        );
        assert!(
            delivery
                .observe("WinA", &inbox(true, "consumed"))
                .is_empty()
        );
        let messages = delivery.observe("WinA", &inbox(true, "pending"));
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("WinB"));
        assert!(delivery.observe("WinA", &inbox(true, "pending")).is_empty());
        assert!(
            delivery
                .observe("WinA", &inbox(false, "pending"))
                .is_empty()
        );
        assert_eq!(
            delivery
                .observe("new-identity", &inbox(true, "pending"))
                .len(),
            1
        );
    }

    #[test]
    fn new_responses_notify_without_replaying_old_history_or_inferring_questions() {
        let mut delivery = SharedAttention {
            started_at_ms: 100,
            ..Default::default()
        };
        let mut responses = inbox(false, "consumed");
        responses.items[0].kind = "finished".into();
        responses.items[0].created_at_ms = 99;
        assert!(delivery.observe("WinA", &responses).is_empty());
        responses.items[0].created_at_ms = 101;
        assert_eq!(delivery.observe("WinA", &responses).len(), 1);
        assert!(delivery.observe("WinA", &responses).is_empty());
        responses.items[0].id = "already-read".into();
        responses.items[0].read_at_ms = Some(102);
        assert!(delivery.observe("WinA", &responses).is_empty());
    }
}
