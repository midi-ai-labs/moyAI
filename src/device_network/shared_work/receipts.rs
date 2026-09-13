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
}
impl ReceiptOperation {
    pub fn path(&self) -> String {
        match self {
            Self::Submit => "jobs".into(),
            Self::Continue { job_id } => format!("jobs/{job_id}/continue"),
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
        if document.version != 1
            || document.receipts.len() > MAX_RECEIPTS
            || document.receipts.iter().any(|r| {
                r.hub.is_empty()
                    || r.hub.len() > 1024
                    || !super::super::stable_id(&r.user_id)
                    || !owners.insert((&r.hub, &r.user_id))
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
        self.receipts
            .iter()
            .find(|r| r.hub == hub && r.user_id == user)
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
        if self.pending(&receipt.hub, &receipt.user_id).is_some() {
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
        project_id: String,
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
        prompt: String,
        input_refs: Vec<String>,
        #[serde(default)]
        start_before_ms: Option<u64>,
    }
    let valid_refs = |refs: &[String]| {
        refs.len() <= 32
            && refs.iter().all(|id| super::super::stable_id(id))
            && refs.iter().collect::<std::collections::BTreeSet<_>>().len() == refs.len()
    };
    if let ReceiptOperation::Continue { job_id } = operation {
        return super::super::stable_id(job_id)
            && serde_json::from_value::<Continuation>(payload.clone()).is_ok_and(|p| {
                super::super::stable_id(&p.request_id)
                    && p.expected_revision > 0
                    && !p.prompt.trim().is_empty()
                    && p.prompt.len() <= 32768
                    && valid_refs(&p.input_refs)
                    && p.start_before_ms
                        .is_none_or(|until| until > 0 && until <= i64::MAX as u64)
            });
    }
    serde_json::from_value::<Submission>(payload.clone()).is_ok_and(|p| {
        [&p.request_id, &p.project_id, &p.environment_id]
            .iter()
            .all(|id| super::super::stable_id(id))
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
        continued.payload = serde_json::json!({"request_id":"next-a","expected_revision":7,"prompt":"追加の解析","input_refs":["asset-a"]});
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
        let mut wrong = continued.clone();
        wrong.operation = ReceiptOperation::Submit;
        reopened.remove_confirmed(&wrong).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_some());
        reopened.remove_confirmed(&continued).unwrap();
        assert!(reopened.pending("trusted-hub", "user-a").is_none());
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
