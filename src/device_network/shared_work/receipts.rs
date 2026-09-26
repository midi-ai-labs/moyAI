//! Bounded, durable idempotency receipts. They contain no authentication token.
use std::io::{Read, Write};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::RequestError;
const MAX_RECEIPTS: usize = 32;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const STORAGE_MESSAGE: &str = "未確認の受付記録を保存・確認できません。記録を保持したまま管理者へ確認してください。新しい仕事の投入は停止しています。";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub hub: String,
    pub user_id: String,
    #[serde(default)]
    pub operation: ReceiptOperation,
    pub payload: Value,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReceiptOperation {
    #[default]
    Submit,
    Continue {
        job_id: String,
    },
    LeaveProject {
        project_id: String,
    },
    StopConversation {
        conversation_id: String,
    },
    StopAllConversation {
        conversation_id: String,
    },
    StopOriginTurn {
        origin_session_ref: String,
        origin_turn_ref: String,
    },
    /// Reserved before the local exact UserStop CAS. It must never be sent
    /// until the durable local request can be observed after a crash.
    StopOriginTurnPrepared {
        origin_session_ref: String,
        origin_turn_ref: String,
    },
    StopOriginConversation {
        origin_session_ref: String,
        through_revision: u64,
    },
}
impl ReceiptOperation {
    pub fn path(&self) -> String {
        match self {
            Self::Submit => "jobs".into(),
            Self::Continue { job_id } => format!("jobs/{job_id}/continue"),
            Self::LeaveProject { project_id } => format!("projects/{project_id}/leave"),
            Self::StopConversation { conversation_id } => {
                format!("conversations/{conversation_id}/services/stop")
            }
            Self::StopAllConversation { conversation_id } => {
                format!("conversations/{conversation_id}/stop")
            }
            Self::StopOriginTurn {
                origin_session_ref,
                origin_turn_ref,
            }
            | Self::StopOriginTurnPrepared {
                origin_session_ref,
                origin_turn_ref,
            } => format!("origins/{origin_session_ref}/turns/{origin_turn_ref}/stop"),
            Self::StopOriginConversation {
                origin_session_ref, ..
            } => format!("origins/{origin_session_ref}/stop"),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    receipts: Vec<Receipt>,
}
#[derive(Default)]
pub(super) struct ReceiptStore {
    path: Utf8PathBuf,
    receipts: Vec<Receipt>,
    blocked: bool,
}
impl ReceiptStore {
    pub fn new(path: Utf8PathBuf) -> Self {
        let mut store = Self {
            path,
            ..Self::default()
        };
        match store.load() {
            Ok(receipts) => store.receipts = receipts,
            Err(_) => store.blocked = true,
        }
        store
    }
    fn load(&self) -> Result<Vec<Receipt>, RequestError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(RequestError::Local(STORAGE_MESSAGE)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        let document: Document =
            serde_json::from_slice(&bytes).map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
        let mut owners = std::collections::BTreeSet::new();
        let mut origin_stops = std::collections::BTreeSet::new();
        let mut origin_conversation_stops = std::collections::BTreeSet::new();
        let mut leave_owners = std::collections::BTreeSet::new();
        if document.version != 1
            || document.receipts.len() > MAX_RECEIPTS
            || document.receipts.iter().any(|r| {
                r.hub.is_empty()
                    || r.hub.len() > 1024
                    || !super::super::stable_id(&r.user_id)
                    || match &r.operation {
                        ReceiptOperation::StopOriginTurn {
                            origin_session_ref,
                            origin_turn_ref,
                        }
                        | ReceiptOperation::StopOriginTurnPrepared {
                            origin_session_ref,
                            origin_turn_ref,
                        } => !origin_stops.insert((&r.hub, origin_session_ref, origin_turn_ref)),
                        ReceiptOperation::StopOriginConversation {
                            origin_session_ref,
                            through_revision,
                        } => !origin_conversation_stops.insert((
                            &r.hub,
                            origin_session_ref,
                            through_revision,
                        )),
                        ReceiptOperation::LeaveProject { .. } => {
                            !leave_owners.insert((&r.hub, &r.user_id))
                        }
                        _ => !owners.insert((&r.hub, &r.user_id)),
                    }
                    || !valid_payload(&r.operation, &r.payload)
            })
        {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        Ok(document.receipts)
    }
    pub fn error(&self) -> Option<&'static str> {
        self.blocked.then_some(STORAGE_MESSAGE)
    }
    pub fn pending(&self, hub: &str, user: &str) -> Option<&Receipt> {
        self.receipts.iter().find(|r| {
            r.hub == hub
                && r.user_id == user
                && !matches!(
                    &r.operation,
                    ReceiptOperation::StopOriginTurn { .. }
                        | ReceiptOperation::StopOriginTurnPrepared { .. }
                        | ReceiptOperation::StopOriginConversation { .. }
                        | ReceiptOperation::LeaveProject { .. }
                )
        })
    }
    pub fn pending_leave(&self, hub: &str, user: &str) -> Option<&Receipt> {
        self.receipts.iter().find(|receipt| {
            receipt.hub == hub
                && receipt.user_id == user
                && matches!(&receipt.operation, ReceiptOperation::LeaveProject { .. })
        })
    }
    pub fn pending_origin_stop(
        &self,
        hub: &str,
        origin_session_ref: &str,
        origin_turn_ref: &str,
    ) -> Option<&Receipt> {
        self.receipts.iter().find(|r| {
            r.hub == hub
                && matches!(&r.operation, ReceiptOperation::StopOriginTurn {
                    origin_session_ref: session,
                    origin_turn_ref: turn,
                } | ReceiptOperation::StopOriginTurnPrepared {
                    origin_session_ref: session,
                    origin_turn_ref: turn,
                } if session == origin_session_ref && turn == origin_turn_ref)
        })
    }
    pub fn pending_origin_stops(&self, hub: &str) -> Vec<Receipt> {
        self.receipts
            .iter()
            .filter(|r| {
                r.hub == hub
                    && matches!(
                        &r.operation,
                        ReceiptOperation::StopOriginTurn { .. }
                            | ReceiptOperation::StopOriginTurnPrepared { .. }
                            | ReceiptOperation::StopOriginConversation { .. }
                    )
            })
            .cloned()
            .collect()
    }
    pub fn pending_origin_conversation_stop(
        &self,
        hub: &str,
        origin_session_ref: &str,
        through_revision: u64,
    ) -> Option<&Receipt> {
        self.receipts.iter().find(|r| {
            r.hub == hub
                && matches!(&r.operation, ReceiptOperation::StopOriginConversation {
                origin_session_ref: session, through_revision: revision,
            } if session == origin_session_ref && *revision == through_revision)
        })
    }
    pub fn has_origin_submission(&self, origin_session_ref: &str) -> Result<bool, RequestError> {
        if self.blocked {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        Ok(self.receipts.iter().any(|receipt| {
            receipt.operation == ReceiptOperation::Submit
                && receipt.payload["origin_session_ref"].as_str() == Some(origin_session_ref)
        }))
    }
    pub fn has_origin_turn_submission(
        &self,
        origin_session_ref: &str,
        origin_turn_ref: &str,
    ) -> Result<bool, RequestError> {
        if self.blocked {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        Ok(self.receipts.iter().any(|receipt| {
            receipt.operation == ReceiptOperation::Submit
                && receipt.payload["origin_session_ref"].as_str() == Some(origin_session_ref)
                && receipt.payload["origin_turn_ref"].as_str() == Some(origin_turn_ref)
        }))
    }
    pub fn insert(&mut self, receipt: Receipt) -> Result<(), RequestError> {
        if self.blocked {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        if self.receipts.len() >= MAX_RECEIPTS {
            return Err(RequestError::Local(
                "未確認の受付記録が上限に達しました。既存の受付を確認してください。",
            ));
        }
        let duplicate = match &receipt.operation {
            ReceiptOperation::StopOriginTurn {
                origin_session_ref,
                origin_turn_ref,
            }
            | ReceiptOperation::StopOriginTurnPrepared {
                origin_session_ref,
                origin_turn_ref,
            } => self
                .pending_origin_stop(&receipt.hub, origin_session_ref, origin_turn_ref)
                .is_some(),
            ReceiptOperation::StopOriginConversation {
                origin_session_ref,
                through_revision,
            } => self
                .pending_origin_conversation_stop(
                    &receipt.hub,
                    origin_session_ref,
                    *through_revision,
                )
                .is_some(),
            ReceiptOperation::LeaveProject { .. } => {
                self.pending_leave(&receipt.hub, &receipt.user_id).is_some()
            }
            _ => self.pending(&receipt.hub, &receipt.user_id).is_some(),
        };
        if duplicate {
            return Err(RequestError::Local(
                "前回の未確認の受付を先に確認してください。",
            ));
        }
        if !valid_payload(&receipt.operation, &receipt.payload) {
            return Err(RequestError::Invalid);
        }
        let mut next = self.receipts.clone();
        next.push(receipt);
        self.commit(next)
    }
    pub fn promote_prepared_origin_stop(&mut self, receipt: &Receipt) -> Result<(), RequestError> {
        let ReceiptOperation::StopOriginTurnPrepared {
            origin_session_ref,
            origin_turn_ref,
        } = &receipt.operation
        else {
            return Err(RequestError::Invalid);
        };
        let mut next = self.receipts.clone();
        let Some(stored) = next.iter_mut().find(|stored| *stored == receipt) else {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        };
        stored.operation = ReceiptOperation::StopOriginTurn {
            origin_session_ref: origin_session_ref.clone(),
            origin_turn_ref: origin_turn_ref.clone(),
        };
        self.commit(next)
    }
    pub fn remove_confirmed(&mut self, receipt: &Receipt) -> Result<(), RequestError> {
        let next = self
            .receipts
            .iter()
            .filter(|r| {
                !(r.hub == receipt.hub
                    && r.user_id == receipt.user_id
                    && r.operation == receipt.operation
                    && r.payload == receipt.payload)
            })
            .cloned()
            .collect();
        self.commit(next)
    }
    fn commit(&mut self, next: Vec<Receipt>) -> Result<(), RequestError> {
        if self.blocked {
            return Err(RequestError::Local(STORAGE_MESSAGE));
        }
        let save = || -> Result<(), RequestError> {
            let parent = self
                .path
                .parent()
                .ok_or(RequestError::Local(STORAGE_MESSAGE))?;
            std::fs::create_dir_all(parent).map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.path.with_extension("lock"))
                .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            fs2::FileExt::try_lock_exclusive(&lock)
                .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            if self.load()? != self.receipts {
                return Err(RequestError::Local(STORAGE_MESSAGE));
            }
            let bytes = serde_json::to_vec(&Document {
                version: 1,
                receipts: next.clone(),
            })
            .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err(RequestError::Local(STORAGE_MESSAGE));
            }
            let mut temporary = tempfile::NamedTempFile::new_in(parent)
                .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                temporary
                    .as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            }
            temporary
                .write_all(&bytes)
                .and_then(|_| temporary.as_file().sync_all())
                .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            temporary
                .persist(&self.path)
                .map_err(|_| RequestError::Local(STORAGE_MESSAGE))?;
            Ok(())
        };
        if let Err(error) = save() {
            self.blocked = true;
            return Err(error);
        }
        self.receipts = next;
        Ok(())
    }
}
fn valid_payload(operation: &ReceiptOperation, payload: &Value) -> bool {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        version: u32,
        prompt: String,
        #[serde(default)]
        input_refs: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Submission {
        request_id: String,
        #[serde(default)]
        origin_session_ref: Option<String>,
        #[serde(default)]
        origin_turn_ref: Option<String>,
        #[serde(default)]
        origin_turn_epoch: Option<u64>,
        project_id: String,
        #[serde(default)]
        project_participation: Option<u64>,
        #[serde(default)]
        conversation_id: Option<String>,
        #[serde(default)]
        revises_job_id: Option<String>,
        #[serde(default)]
        expected_revised_revision: Option<u64>,
        environment_id: String,
        title: String,
        input: Input,
        descendant_budget: u32,
        #[serde(default)]
        start_before_ms: Option<u64>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Continuation {
        request_id: String,
        expected_revision: u64,
        #[serde(default)]
        project_participation: Option<u64>,
        #[serde(default)]
        conversation_epoch: u64,
        prompt: String,
        input_refs: Vec<String>,
        #[serde(default)]
        start_before_ms: Option<u64>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StopConversation {
        project_id: String,
        request_id: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LeaveProject {
        expected_participation_generation: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StopOriginTurn {
        request_id: String,
        origin_turn_revision: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StopOriginConversation {
        request_id: String,
        through_revision: u64,
    }
    let valid_refs = |refs: &[String]| {
        refs.len() <= 32
            && refs.iter().all(|id| super::super::stable_id(id))
            && refs.iter().collect::<std::collections::BTreeSet<_>>().len() == refs.len()
    };
    if let ReceiptOperation::StopConversation { conversation_id }
    | ReceiptOperation::StopAllConversation { conversation_id } = operation
    {
        return super::super::stable_id(conversation_id)
            && serde_json::from_value::<StopConversation>(payload.clone()).is_ok_and(|p| {
                super::super::stable_id(&p.project_id) && super::super::stable_id(&p.request_id)
            });
    }
    if let ReceiptOperation::LeaveProject { project_id } = operation {
        return super::super::stable_id(project_id)
            && serde_json::from_value::<LeaveProject>(payload.clone())
                .is_ok_and(|value| value.expected_participation_generation > 0);
    }
    if let ReceiptOperation::StopOriginTurn {
        origin_session_ref,
        origin_turn_ref,
    }
    | ReceiptOperation::StopOriginTurnPrepared {
        origin_session_ref,
        origin_turn_ref,
    } = operation
    {
        return super::super::stable_id(origin_session_ref)
            && super::super::stable_id(origin_turn_ref)
            && serde_json::from_value::<StopOriginTurn>(payload.clone()).is_ok_and(|p| {
                super::super::stable_id(&p.request_id) && p.origin_turn_revision > 0
            });
    }
    if let ReceiptOperation::StopOriginConversation {
        origin_session_ref,
        through_revision,
    } = operation
    {
        return super::super::stable_id(origin_session_ref)
            && serde_json::from_value::<StopOriginConversation>(payload.clone()).is_ok_and(|p| {
                super::super::stable_id(&p.request_id)
                    && p.through_revision == *through_revision
                    && p.through_revision > 0
            });
    }
    if let ReceiptOperation::Continue { job_id } = operation {
        return super::super::stable_id(job_id)
            && serde_json::from_value::<Continuation>(payload.clone()).is_ok_and(|p| {
                super::super::stable_id(&p.request_id)
                    && p.expected_revision > 0
                    && p.project_participation
                        .is_none_or(|generation| generation > 0)
                    && p.conversation_epoch <= i64::MAX as u64
                    && !p.prompt.trim().is_empty()
                    && p.prompt.len() <= 32768
                    && valid_refs(&p.input_refs)
                    && p.start_before_ms
                        .is_none_or(|until| until > 0 && until <= i64::MAX as u64)
            });
    }
    serde_json::from_value::<Submission>(payload.clone()).is_ok_and(|p| {
        [&p.request_id, &p.project_id]
            .iter()
            .all(|id| super::super::stable_id(id))
            && p.origin_session_ref
                .as_deref()
                .is_none_or(super::super::stable_id)
            && match (p.origin_turn_ref.as_deref(), p.origin_turn_epoch) {
                (None, None) => true,
                (Some(turn), Some(epoch)) => {
                    p.origin_session_ref.is_some() && super::super::stable_id(turn) && epoch > 0
                }
                _ => false,
            }
            && (p.environment_id.is_empty() || super::super::stable_id(&p.environment_id))
            && p.project_participation
                .is_none_or(|generation| generation > 0)
            && match (
                p.conversation_id.as_deref(),
                p.revises_job_id.as_deref(),
                p.expected_revised_revision,
            ) {
                (None, None, None) => true,
                (Some(conversation), Some(job), Some(revision)) => {
                    super::super::stable_id(conversation)
                        && super::super::stable_id(job)
                        && revision > 0
                        && p.origin_session_ref.is_none()
                }
                _ => false,
            }
            && !p.title.trim().is_empty()
            && p.title.len() <= 256
            && ((p.input.version == 1 && p.input.input_refs.is_empty()) || p.input.version == 2)
            && valid_refs(&p.input.input_refs)
            && !p.input.prompt.trim().is_empty()
            && p.input.prompt.len() <= 32768
            && p.descendant_budget <= 8
            && p.start_before_ms
                .is_none_or(|until| until > 0 && until <= i64::MAX as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_accepts_hub_placement_and_opaque_local_parent_only() {
        let mut payload = serde_json::json!({
            "request_id":"request-b","origin_session_ref":"01K6SESSION",
            "project_id":"project-a","environment_id":"", "title":"依頼",
            "input":{"version":2,"prompt":"WinBで確認","input_refs":[]},
            "descendant_budget":8
        });
        assert!(valid_payload(&ReceiptOperation::Submit, &payload));
        payload["origin_turn_ref"] = serde_json::json!("turn-a");
        assert!(!valid_payload(&ReceiptOperation::Submit, &payload));
        payload["origin_turn_epoch"] = serde_json::json!(1);
        assert!(valid_payload(&ReceiptOperation::Submit, &payload));
        payload["origin_session_ref"] = serde_json::json!("secret or malformed");
        assert!(!valid_payload(&ReceiptOperation::Submit, &payload));
    }
    #[test]
    fn revised_submission_requires_exact_conversation_and_revision_tuple() {
        let mut payload = serde_json::json!({
            "request_id":"request-r","project_id":"project-a","project_participation":2,
            "environment_id":"","conversation_id":"conversation-a",
            "revises_job_id":"job-old","expected_revised_revision":4,
            "title":"Corrected request","input":{"version":2,"prompt":"Use WinB","input_refs":[]},
            "descendant_budget":8
        });
        assert!(valid_payload(&ReceiptOperation::Submit, &payload));
        payload
            .as_object_mut()
            .unwrap()
            .remove("expected_revised_revision");
        assert!(!valid_payload(&ReceiptOperation::Submit, &payload));
        assert!(valid_payload(
            &ReceiptOperation::LeaveProject {
                project_id: "project-a".into()
            },
            &serde_json::json!({"expected_participation_generation":2})
        ));
    }
    fn receipt(user: &str) -> Receipt {
        Receipt {
            hub: "trusted-hub".into(),
            user_id: user.into(),
            operation: ReceiptOperation::Submit,
            payload: serde_json::json!({"request_id":"request-a","project_id":"project-a","environment_id":"env-a","title":"依頼","input":{"version":1,"prompt":"解析"},"descendant_budget":8}),
        }
    }
    #[test]
    fn receipt_survives_restart_and_is_only_selected_for_the_same_hub_and_human() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let mut store = ReceiptStore::new(path.clone());
        store.insert(receipt("user-a")).unwrap();
        let mut reopened = ReceiptStore::new(path.clone());
        assert!(reopened.pending("trusted-hub", "user-b").is_none());
        assert!(reopened.pending("other-hub", "user-a").is_none());
        assert_eq!(
            reopened.pending("trusted-hub", "user-a").unwrap().payload["request_id"],
            "request-a"
        );
        reopened.remove_confirmed(&receipt("user-a")).unwrap();
        assert!(
            ReceiptStore::new(path)
                .pending("trusted-hub", "user-a")
                .is_none()
        );
    }
    #[test]
    fn corrupt_receipts_are_preserved_and_prevent_new_submission_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        std::fs::write(&path, b"corrupt receipt").unwrap();
        let mut store = ReceiptStore::new(path.clone());
        assert!(store.error().is_some());
        assert!(store.insert(receipt("user-a")).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"corrupt receipt");
    }
    #[test]
    fn continuation_receipt_restores_exact_endpoint_and_cannot_be_retired_as_submission() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let mut continued = receipt("user-a");
        continued.operation = ReceiptOperation::Continue {
            job_id: "finished-a".into(),
        };
        continued.payload = serde_json::json!({"request_id":"next-a","expected_revision":7,"conversation_epoch":2,"prompt":"追加の解析","input_refs":["asset-a"]});
        ReceiptStore::new(path.clone())
            .insert(continued.clone())
            .unwrap();
        let mut reopened = ReceiptStore::new(path);
        assert_eq!(
            reopened
                .pending("trusted-hub", "user-a")
                .unwrap()
                .operation
                .path(),
            "jobs/finished-a/continue"
        );
        assert_eq!(
            reopened.pending("trusted-hub", "user-a").unwrap().payload["conversation_epoch"],
            2
        );
        let mut wrong = continued.clone();
        wrong.operation = ReceiptOperation::Submit;
        reopened.remove_confirmed(&wrong).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_some());
        reopened.remove_confirmed(&continued).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_none());
    }

    #[test]
    fn conversation_stop_receipt_replays_exact_request_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let mut stopped = receipt("user-a");
        stopped.operation = ReceiptOperation::StopConversation {
            conversation_id: "conversation-a".into(),
        };
        stopped.payload = serde_json::json!({"project_id":"project-a","request_id":"stop-once-a"});
        ReceiptStore::new(path.clone())
            .insert(stopped.clone())
            .unwrap();
        let mut reopened = ReceiptStore::new(path);
        let pending = reopened.pending("trusted-hub", "user-a").unwrap();
        assert_eq!(
            pending.operation.path(),
            "conversations/conversation-a/services/stop"
        );
        assert_eq!(pending.payload["request_id"], "stop-once-a");
        let mut wrong = stopped.clone();
        wrong.payload["request_id"] = serde_json::json!("stop-later-b");
        reopened.remove_confirmed(&wrong).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_some());
        reopened.remove_confirmed(&stopped).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_none());
        let mut bad_payload = stopped.payload;
        bad_payload["extra"] = serde_json::json!(true);
        assert!(!valid_payload(&stopped.operation, &bad_payload));
    }
    #[test]
    fn project_conversation_full_stop_uses_a_distinct_replay_target() {
        let operation = ReceiptOperation::StopAllConversation {
            conversation_id: "conversation-a".into(),
        };
        assert_eq!(operation.path(), "conversations/conversation-a/stop");
        assert!(valid_payload(
            &operation,
            &serde_json::json!({"project_id":"project-a","request_id":"stop-all-once"})
        ));
        assert!(!valid_payload(
            &operation,
            &serde_json::json!({"project_id":"project-a","request_id":"stop-all-once","extra":true})
        ));
    }
    #[test]
    fn prepared_origin_stop_survives_restart_without_becoming_a_dispatchable_stop() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let prepared = Receipt {
            hub: "trusted-hub".into(),
            user_id: "device".into(),
            operation: ReceiptOperation::StopOriginTurnPrepared {
                origin_session_ref: "session-a".into(),
                origin_turn_ref: "turn-a".into(),
            },
            payload: serde_json::json!({"request_id":"stop-turn-a","origin_turn_revision":4}),
        };
        ReceiptStore::new(path.clone())
            .insert(prepared.clone())
            .unwrap();
        let mut restored = ReceiptStore::new(path.clone());
        assert!(restored.pending_origin_stops("trusted-hub") == vec![prepared.clone()]);
        assert!(
            restored
                .pending_origin_stop("trusted-hub", "session-a", "turn-a")
                .is_some()
        );
        let mut duplicate_ready = prepared.clone();
        duplicate_ready.operation = ReceiptOperation::StopOriginTurn {
            origin_session_ref: "session-a".into(),
            origin_turn_ref: "turn-a".into(),
        };
        assert!(restored.insert(duplicate_ready.clone()).is_err());
        restored.promote_prepared_origin_stop(&prepared).unwrap();
        let reopened = ReceiptStore::new(path);
        assert!(reopened.pending_origin_stops("trusted-hub") == vec![duplicate_ready]);
    }
    #[test]
    fn pending_submission_is_scoped_to_its_exact_origin_turn() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let mut submitted = receipt("device");
        submitted.payload["origin_session_ref"] = serde_json::json!("session-a");
        submitted.payload["origin_turn_ref"] = serde_json::json!("turn-old");
        submitted.payload["origin_turn_epoch"] = serde_json::json!(1);
        let mut store = ReceiptStore::new(path);
        store.insert(submitted).unwrap();
        assert!(store.has_origin_submission("session-a").unwrap());
        assert!(
            store
                .has_origin_turn_submission("session-a", "turn-old")
                .unwrap()
        );
        assert!(
            !store
                .has_origin_turn_submission("session-a", "turn-new")
                .unwrap()
        );
    }
    #[test]
    fn legacy_receipt_stays_submission_and_v2_rejects_duplicate_input_refs() {
        let mut value = serde_json::to_value(receipt("user-a")).unwrap();
        value.as_object_mut().unwrap().remove("operation");
        let restored: Receipt = serde_json::from_value(value).unwrap();
        assert_eq!(restored.operation, ReceiptOperation::Submit);
        let mut payload = restored.payload;
        payload["input"] =
            serde_json::json!({"version":2,"prompt":"read","input_refs":["asset-a","asset-a"]});
        assert!(!valid_payload(&ReceiptOperation::Submit, &payload));
    }

    #[test]
    fn submission_deadline_survives_restart_without_extending_an_expired_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(directory.path().join("receipts.json")).unwrap();
        let deadline = super::super::submission_start_deadline(None, 100).unwrap();
        assert_eq!(deadline, 86_400_100);
        assert_eq!(
            super::super::submission_start_deadline(Some(120), 100).unwrap(),
            120
        );
        assert!(super::super::submission_start_deadline(Some(100), 100).is_err());
        assert!(super::super::submission_start_deadline(Some(u64::MAX), 100).is_err());

        let mut submitted = receipt("user-a");
        submitted.payload["start_before_ms"] = serde_json::json!(deadline);
        ReceiptStore::new(path.clone())
            .insert(submitted.clone())
            .unwrap();
        // Loading a receipt never revalidates its timestamp against today's clock:
        // the Hub must reconcile the original request even after its deadline.
        let reopened = ReceiptStore::new(path);
        assert_eq!(
            reopened.pending("trusted-hub", "user-a").unwrap().payload,
            submitted.payload
        );

        let continued = serde_json::json!({"request_id":"next-deadline","expected_revision":2,"prompt":"Continue","input_refs":[],"start_before_ms":120});
        assert!(valid_payload(
            &ReceiptOperation::Continue {
                job_id: "previous-job".into()
            },
            &continued
        ));
        let mut malformed = submitted.payload;
        malformed["start_before_ms"] = serde_json::json!(0);
        assert!(!valid_payload(&ReceiptOperation::Submit, &malformed));
    }
}
