//! Hub data is fetched under the exact assignment, then materialized through workspace handles.
use super::{SharedInput, journal::Entry, transport::SharedClient};
use crate::{
    runner::RunnerError,
    workspace::{AccessKind, PathGuard, Workspace, WorkspaceDiscovery},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Debug, Deserialize)]
struct Asset {
    id: String,
    project_id: String,
    job_id: Option<String>,
    kind: String,
    name: String,
    sha256: String,
    byte_length: u64,
}
#[derive(Debug, Deserialize)]
struct Download {
    asset: Asset,
    content_base64: String,
}
pub(super) struct Prepared {
    pub prompt: String,
    pub archive: Option<Value>,
    pub continuation: Option<crate::agent::shared::SharedContinuation>,
}
fn error(value: impl std::fmt::Display) -> RunnerError {
    RunnerError::new(value.to_string())
}
fn bytes(download: &Download, max: usize) -> Result<Vec<u8>, RunnerError> {
    if download.asset.byte_length > max as u64
        || download.content_base64.len() > max.div_ceil(3) * 4
    {
        return Err(error("shared asset size exceeds its bound"));
    }
    let bytes = STANDARD.decode(&download.content_base64).map_err(error)?;
    if bytes.len() as u64 != download.asset.byte_length
        || format!("{:x}", Sha256::digest(&bytes)) != download.asset.sha256
    {
        return Err(error(
            "shared asset checksum differs from its immutable reference",
        ));
    }
    Ok(bytes)
}
fn path(name: &str) -> Result<(), RunnerError> {
    if name.is_empty()
        || name.len() > 512
        || name.contains(['\\', ':'])
        || name.starts_with('/')
        || name.split('/').any(|part| {
            part.is_empty() || matches!(part, "." | "..") || part.chars().any(char::is_control)
        })
    {
        return Err(error("shared asset name is not a relative workspace path"));
    }
    Ok(())
}
pub(super) async fn prepare(
    client: &SharedClient,
    entry: &Entry,
    input: &SharedInput,
) -> Result<Prepared, RunnerError> {
    let attempt = &entry.assignment.attempt_id;
    let job = &entry.assignment.job;
    let mut prompt = input.prompt.clone();
    let mut files = Vec::new();
    for id in &input.input_refs {
        let download: Download = client
            .preparation_read(&format!("/v1/shared/runner/attempts/{attempt}/assets/{id}"))
            .await
            .map_err(RunnerError::from)?;
        if download.asset.id != *id
            || download.asset.project_id != job.project_id
            || download.asset.kind == "archive"
        {
            return Err(error("Hub asset does not match the assigned project input"));
        }
        path(&download.asset.name)?;
        files.push((
            download.asset.name.clone(),
            bytes(&download, 8 * 1024 * 1024)?,
        ));
    }
    if !files.is_empty() {
        let config = crate::config::ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&entry.mapping.directory, &config)
            .map_err(error)?;
        let directory = format!(".moyai-shared-inputs-{}", job.id);
        let guarded = PathGuard::require_path(
            &workspace,
            camino::Utf8Path::new(&directory),
            AccessKind::Edit,
        )
        .map_err(error)?;
        if guarded.absolute.exists() {
            for (name, expected) in &files {
                let guarded = PathGuard::require_path(
                    &workspace,
                    &camino::Utf8PathBuf::from(&directory).join(name),
                    AccessKind::Read,
                )
                .map_err(error)?;
                let file = PathGuard::open_validated_read_file(&guarded).map_err(error)?;
                let mut actual = Vec::new();
                file.take(8 * 1024 * 1024 + 1)
                    .read_to_end(&mut actual)
                    .map_err(error)?;
                if &actual != expected {
                    return Err(error(
                        "existing shared input snapshot differs; it was not overwritten",
                    ));
                }
            }
        } else {
            crate::tool::write_support::create_new_bytes_tree(&guarded, &files).map_err(error)?;
        }
        prompt.push_str("\n\nShared input snapshots (read these files; preserve the originals):\n");
        for (name, _) in &files {
            prompt.push_str(&format!("- {directory}/{name}\n"));
        }
    }
    let download: Option<Download> = client
        .preparation_read(&format!("/v1/shared/runner/attempts/{attempt}/archive"))
        .await
        .map_err(RunnerError::from)?;
    let mut archive = None;
    let mut continuation = None;
    if let Some(download) = download {
        if download.asset.project_id != job.project_id || download.asset.kind != "archive" {
            return Err(error("Hub continuation archive belongs to another project"));
        }
        let value: Value =
            serde_json::from_slice(&bytes(&download, 64 * 1024 * 1024)?).map_err(error)?;
        if download.asset.job_id.as_deref() == Some(job.id.as_str()) {
            if job.checkpoint.is_some() {
                archive = Some(value);
            } else if job.continued_from.is_some() {
                return Err(error("continuation archive is not the exact predecessor"));
            }
        } else {
            if job.checkpoint.is_some() {
                return Err(error("checkpoint archive belongs to another job"));
            }
            if download.asset.job_id.as_deref() != job.continued_from.as_deref() {
                return Err(error("continuation archive is not the exact predecessor"));
            }
            continuation = Some(crate::agent::shared::SharedContinuation {
                previous_job_id: download
                    .asset
                    .job_id
                    .ok_or_else(|| error("continuation archive has no business identity"))?,
                archive: value,
            });
        }
    } else if job.continued_from.is_some()
        || job
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint["version"] == 2)
    {
        return Err(error("portable checkpoint has no durable Hub archive"));
    }
    Ok(Prepared {
        prompt,
        archive,
        continuation,
    })
}

/// Called after real drain and before reporting release. The immutable request IDs make both
/// a lost upload acknowledgement and a lost final-report acknowledgement safe to retry.
pub(super) async fn persist(
    client: &SharedClient,
    host: &crate::runner::RunnerHost,
    entry: &Entry,
) -> Result<(), RunnerError> {
    let uploads = freeze(host, entry)?;
    for upload in uploads {
        let _: Value = client
            .request(
                &format!(
                    "/v1/shared/runner/attempts/{}/assets",
                    entry.assignment.attempt_id
                ),
                Some(&upload),
            )
            .await
            .map_err(RunnerError::from)?;
    }
    Ok(())
}
fn spool(host: &crate::runner::RunnerHost) -> Result<rusqlite::Connection, RunnerError> {
    let db = rusqlite::Connection::open(
        host.inner
            .process
            .store()
            .paths()
            .data_dir
            .join("runner-shared-payloads.sqlite3"),
    )
    .map_err(error)?;
    db.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(error)?;
    db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS outcome_payloads(attempt_id TEXT PRIMARY KEY,report_sha256 TEXT NOT NULL,payload_json TEXT NOT NULL);").map_err(error)?;
    Ok(db)
}

/// Once payload publication has begun, the outcome report is an exact retry
/// receipt. A stop crossing that boundary must drain the service and resend the
/// same report; changing its result would conflict with both this spool and a
/// possibly accepted Hub report whose acknowledgement was lost.
pub(super) fn report_frozen(
    host: &crate::runner::RunnerHost,
    entry: &Entry,
) -> Result<bool, RunnerError> {
    let saved: Option<String> = spool(host)?
        .query_row(
            "SELECT report_sha256 FROM outcome_payloads WHERE attempt_id=?1",
            [&entry.assignment.attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(saved) = saved else {
        return Ok(false);
    };
    let current = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&entry.report).map_err(error)?)
    );
    if saved != current {
        return Err(error("shared outcome data is bound to a different report"));
    }
    Ok(true)
}
fn freeze(host: &crate::runner::RunnerHost, entry: &Entry) -> Result<Vec<Value>, RunnerError> {
    let mut db = spool(host)?;
    let tx = db.transaction().map_err(error)?;
    let report_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&entry.report).map_err(error)?)
    );
    let saved: Option<(String, String)> = tx
        .query_row(
            "SELECT report_sha256,payload_json FROM outcome_payloads WHERE attempt_id=?1",
            [&entry.assignment.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((hash, payload)) = saved {
        if hash != report_hash {
            return Err(error("shared outcome data is bound to a different report"));
        }
        return serde_json::from_str(&payload).map_err(error);
    }
    let uploads = collect(host, entry)?;
    let payload = serde_json::to_string(&uploads).map_err(error)?;
    if payload.len() > 192 * 1024 * 1024 {
        return Err(error("shared outcome payload exceeds 192 MiB"));
    }
    tx.execute(
        "INSERT INTO outcome_payloads(attempt_id,report_sha256,payload_json) VALUES(?1,?2,?3)",
        params![entry.assignment.attempt_id, report_hash, payload],
    )
    .map_err(error)?;
    tx.commit().map_err(error)?;
    Ok(uploads)
}
pub(super) fn settled(host: &crate::runner::RunnerHost, entry: &Entry) -> Result<(), RunnerError> {
    spool(host)?
        .execute(
            "DELETE FROM outcome_payloads WHERE attempt_id=?1",
            [&entry.assignment.attempt_id],
        )
        .map_err(error)?;
    Ok(())
}
fn collect(host: &crate::runner::RunnerHost, entry: &Entry) -> Result<Vec<Value>, RunnerError> {
    let store = host.inner.process.store();
    let Some(archive) = store
        .session_repo()
        .export_shared_archive(&entry.assignment.job.id, store.paths())
        .map_err(error)?
    else {
        return Ok(vec![]);
    };
    let content = serde_json::to_vec(&archive).map_err(error)?;
    let attempt = &entry.assignment.attempt_id;
    let upload = json!({"generation":entry.assignment.generation,"kind":"archive","base_sha256":null,
        "upload":{"request_id":format!("{attempt}:archive"),"name":"canonical-history.json","content_base64":STANDARD.encode(&content),"sha256":format!("{:x}",Sha256::digest(&content))}});
    let published = published_artifacts(&archive, attempt, entry.assignment.generation)?;
    let names = published
        .iter()
        .filter_map(|u| u["upload"]["name"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut uploads = vec![upload];
    let config = crate::config::ResolvedConfig::default();
    let workspace = WorkspaceDiscovery::discover_fixed_root(&entry.mapping.directory, &config)
        .map_err(error)?;
    uploads.extend(changed_file_uploads(
        &archive,
        &workspace,
        &names,
        attempt,
        entry.assignment.generation,
    )?);
    uploads.extend(published);
    Ok(uploads)
}
fn changed_file_uploads(
    archive: &Value,
    workspace: &Workspace,
    names: &std::collections::HashSet<&str>,
    attempt: &str,
    generation: u64,
) -> Result<Vec<Value>, RunnerError> {
    let mut changes = std::collections::BTreeMap::<String, (Option<String>, Option<String>)>::new();
    if let Some(rows) = archive["tables"]["file_changes"].as_array() {
        for row in rows {
            let Some(name) = row["path_after"].as_str() else {
                continue;
            };
            let relative = file_change_name(name, &workspace.root)?;
            let change = changes
                .entry(relative)
                .or_insert_with(|| (row["before_sha256"].as_str().map(str::to_owned), None));
            change.1 = row["after_sha256"].as_str().map(str::to_owned);
        }
    }
    if changes.len() > 128 {
        return Err(error(
            "shared result has more than 128 changed artifact files",
        ));
    }
    let mut uploads = Vec::new();
    for (name, (base, _)) in changes {
        if names.contains(name.as_str()) {
            continue;
        }
        let guarded =
            PathGuard::require_path(&workspace, camino::Utf8Path::new(&name), AccessKind::Read)
                .map_err(error)?;
        if !guarded.absolute.exists() {
            continue;
        }
        let mut content = Vec::new();
        PathGuard::open_validated_read_file(&guarded)
            .map_err(error)?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut content)
            .map_err(error)?;
        if content.len() > 8 * 1024 * 1024 {
            return Err(error("shared artifact exceeds 8 MiB"));
        }
        let name_hash = format!("{:x}", Sha256::digest(name.as_bytes()));
        let upload = json!({"generation":generation,"kind":"artifact","base_sha256":base,
            "upload":{"request_id":format!("{attempt}:file:{}",&name_hash[..32]),"name":name,"sha256":format!("{:x}",Sha256::digest(&content)),"content_base64":STANDARD.encode(content)}});
        uploads.push(upload);
    }
    Ok(uploads)
}
fn file_change_name(name: &str, workspace_root: &camino::Utf8Path) -> Result<String, RunnerError> {
    let path = camino::Utf8Path::new(name);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace_root).map_err(error)?
    } else {
        path
    };
    // The Hub asset name is portable even when the local history used Windows separators.
    #[cfg(windows)]
    let name = relative.as_str().replace('\\', "/");
    #[cfg(not(windows))]
    let name = relative.as_str().to_owned();
    path_name(&name)?;
    Ok(name)
}
fn path_name(name: &str) -> Result<(), RunnerError> {
    path(name)
}

fn published_artifacts(
    archive: &Value,
    attempt: &str,
    generation: u64,
) -> Result<Vec<Value>, RunnerError> {
    let Some(rows) = archive["tables"]["protocol_history_items"].as_array() else {
        return Err(error("shared history is missing"));
    };
    let history = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<Value>(
                row["payload_json"]
                    .as_str()
                    .ok_or_else(|| error("shared history payload is missing"))?,
            )
            .map(|payload| (&row["id"], payload))
            .map_err(error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tools = archive["tables"]["tool_calls"]
        .as_array()
        .ok_or_else(|| error("shared tool records are missing"))?;
    let sidecars = archive["sidecars"]
        .as_array()
        .ok_or_else(|| error("shared output snapshots are missing"))?;
    let mut uploads = Vec::new();
    for (history_id, call) in &history {
        if call["kind"] != "tool_call" || call["tool_name"] != "shared_publish_artifact" {
            continue;
        }
        let Some(tool) = tools
            .iter()
            .find(|tool| tool["history_item_id"] == **history_id && tool["status"] == "completed")
        else {
            continue;
        };
        let output = history
            .iter()
            .map(|(_, payload)| payload)
            .find(|p| {
                p["kind"] == "tool_output"
                    && p["call_id"] == call["call_id"]
                    && p["success"] != false
            })
            .ok_or_else(|| error("published artifact has no canonical successful result"))?;
        // Agent output metadata wraps the handler's result under `tool_metadata`.
        let metadata = &output["metadata"]["tool_metadata"]["artifact"];
        let name = metadata["name"]
            .as_str()
            .ok_or_else(|| error("published artifact name is missing"))?;
        path(name)?;
        let snapshot = tool["truncated_output_path"]
            .as_str()
            .ok_or_else(|| error("published artifact snapshot is missing"))?;
        let sidecar = sidecars
            .iter()
            .find(|sidecar| sidecar["original_path"] == snapshot)
            .ok_or_else(|| error("published artifact bytes are missing"))?;
        let encoded = sidecar["content_base64"]
            .as_str()
            .ok_or_else(|| error("published artifact encoding is missing"))?;
        if encoded.len() > (8 * 1024 * 1024usize).div_ceil(3) * 4 {
            return Err(error("published artifact exceeds 8 MiB"));
        }
        let content = STANDARD.decode(encoded).map_err(error)?;
        let hash = format!("{:x}", Sha256::digest(&content));
        if content.len() > 8 * 1024 * 1024
            || metadata["byte_length"].as_u64() != Some(content.len() as u64)
            || metadata["sha256"] != hash
            || sidecar["sha256"] != hash
        {
            return Err(error(
                "published artifact checksum differs from its canonical result",
            ));
        }
        let call_hash = format!(
            "{:x}",
            Sha256::digest(
                call["call_id"]
                    .as_str()
                    .ok_or_else(|| error("published artifact call is missing"))?
                    .as_bytes()
            )
        );
        uploads.push(json!({"generation":generation,"kind":"artifact","base_sha256":null,"upload":{"request_id":format!("{attempt}:published:{}",&call_hash[..32]),"name":name,"sha256":hash,"content_base64":encoded}}));
    }
    Ok(uploads)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn nested_windows_change_publishes_portable_name_and_original_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(temp.path().join("workspace")).unwrap();
        std::fs::create_dir_all(root.join("todo_app")).unwrap();
        let content = b"nested Flask app\n";
        std::fs::write(root.join("todo_app/app.py"), content).unwrap();
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            &root,
            &crate::config::ResolvedConfig::default(),
        )
        .unwrap();
        let archive = json!({"tables":{"file_changes":[{
            "path_after":"todo_app\\app.py","before_sha256":null,
            "after_sha256":format!("{:x}",Sha256::digest(content))
        }]}});
        let uploads = changed_file_uploads(
            &archive,
            &workspace,
            &std::collections::HashSet::new(),
            "nested-attempt",
            1,
        )
        .unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0]["upload"]["name"], "todo_app/app.py");
        assert_eq!(
            STANDARD
                .decode(uploads[0]["upload"]["content_base64"].as_str().unwrap())
                .unwrap(),
            content
        );
    }
    #[test]
    fn changed_file_names_are_portable_and_stay_within_the_workspace() {
        let root = camino::Utf8PathBuf::from_path_buf(std::env::current_dir().unwrap())
            .unwrap()
            .join("workspace");
        let nested = root.join("todo_app/app.py");
        assert_eq!(
            file_change_name(nested.as_str(), &root).unwrap(),
            "todo_app/app.py"
        );
        assert_eq!(
            file_change_name("todo_app/app.py", &root).unwrap(),
            "todo_app/app.py"
        );
        assert!(path_name("todo_app\\app.py").is_err());
        assert!(file_change_name("../outside.py", &root).is_err());
        assert!(
            file_change_name(
                root.with_file_name("foreign").join("app.py").as_str(),
                &root
            )
            .is_err()
        );
        #[cfg(windows)]
        {
            assert_eq!(
                file_change_name("todo_app\\app.py", &root).unwrap(),
                "todo_app/app.py"
            );
            assert!(file_change_name("todo_app\\..\\outside.py", &root).is_err());
        }
        #[cfg(not(windows))]
        assert!(file_change_name("literal\\name.py", &root).is_err());
    }
    #[test]
    fn shared_artifact_runner_upload_uses_canonical_binary_snapshot_and_checks_ownership() {
        let content = [0u8, 255, 42];
        let hash = format!("{:x}", Sha256::digest(content));
        let mut archive = json!({"tables":{"protocol_history_items":[
            {"id":"call-history","payload_json":json!({"kind":"tool_call","tool_name":"shared_publish_artifact","call_id":"call"}).to_string()},
            {"id":"result-history","payload_json":json!({"kind":"tool_output","call_id":"call","status":"completed","success":true,"metadata":{"success":true,"tool_metadata":{"artifact":{"name":"results/解.bin","sha256":hash,"byte_length":content.len()}}}}).to_string()}
        ],"tool_calls":[{"history_item_id":"call-history","status":"completed","truncated_output_path":"original-snapshot"}]},
            "sidecars":[{"original_path":"original-snapshot","sha256":hash,"content_base64":STANDARD.encode(content)}]});
        let uploads = published_artifacts(&archive, "attempt", 7).unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0]["generation"], 7);
        assert_eq!(uploads[0]["upload"]["name"], "results/解.bin");
        assert_eq!(uploads[0]["upload"]["sha256"], hash);
        assert_eq!(
            uploads,
            published_artifacts(&archive, "attempt", 7).unwrap(),
            "repeated collection must keep the immutable upload identity and bytes"
        );
        assert_eq!(
            STANDARD
                .decode(uploads[0]["upload"]["content_base64"].as_str().unwrap())
                .unwrap(),
            content
        );
        let mut missing_snapshot = archive.clone();
        missing_snapshot["sidecars"] = json!([]);
        assert!(published_artifacts(&missing_snapshot, "attempt", 7).is_err());
        archive["sidecars"][0]["content_base64"] = json!(STANDARD.encode(b"changed source"));
        assert!(published_artifacts(&archive, "attempt", 7).is_err());
        archive["tables"]["tool_calls"][0]["status"] = json!("failed");
        assert!(
            published_artifacts(&archive, "attempt", 7)
                .unwrap()
                .is_empty()
        );
    }
}
