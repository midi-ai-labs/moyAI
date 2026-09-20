use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};

const MAX_FILE: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkAsset {
    pub id: String,
    pub project_id: String,
    pub job_id: Option<String>,
    pub kind: String,
    pub name: String,
    pub sha256: String,
    pub byte_length: u64,
    pub created_at_ms: u64,
    pub version: u32,
    pub base_sha256: Option<String>,
    #[serde(default)]
    pub purged_at_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Download {
    asset: WorkAsset,
    content_base64: String,
}
impl SharedWorkProjection {
    fn can_prepare_sample(&self, project_id: &str) -> bool {
        self.selected_job_id.is_none()
            && self.inputs.is_empty()
            && !self.submission_uncertain
            && self.submission_storage_error.is_none()
            && self
                .projects
                .iter()
                .any(|p| p.id == project_id && p.can_submit)
    }
    fn input_slot(&self, name: &str) -> Result<Option<usize>, RequestError> {
        // Match the Hub's portable, case-insensitive input-name collision rule.
        let name = name.to_lowercase();
        let existing = self
            .inputs
            .iter()
            .position(|asset| asset.name.to_lowercase() == name);
        if existing.is_none() && self.inputs.len() >= 32 {
            return Err(RequestError::Local("入力ファイルは32件までです。"));
        }
        Ok(existing)
    }
    fn attach_input(&mut self, asset: WorkAsset) -> Result<(), RequestError> {
        let existing = self.input_slot(&asset.name)?;
        self.feedback = Some(format!(
            "{}: {}",
            if existing.is_some() {
                "同名の添付を新しい内容に置き換えました"
            } else {
                "添付しました"
            },
            asset.name
        ));
        if let Some(index) = existing {
            self.inputs[index] = asset;
        } else {
            self.inputs.push(asset);
        }
        Ok(())
    }
}
impl DeviceNetworkService {
    /// Explicitly attach public sample data through the ordinary project asset API.
    /// Submission still requires the user's normal Send action and durable receipt.
    pub(super) async fn shared_prepare_sample(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        project_id: &str,
    ) -> Result<(), RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_project(project_id, generation, query)?;
            if !runtime.view.can_prepare_sample(project_id) {
                return Err(RequestError::Local(
                    "添付のない新しいチャットで、依頼できるプロジェクトを選んでください。",
                ));
            }
        }
        let bytes = b"value\n10\n20\n30\n";
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let asset: WorkAsset = request(
            &connection.client,
            &format!("projects/{project_id}/assets"),
            Some(token),
            Some(json!({
                "request_id": format!("sample-{}", ulid::Ulid::new()),
                "name": "moyai-sample-numbers.csv", "sha256": sha256,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes)
            })),
            &[],
        )
        .await?;
        if asset.project_id != project_id
            || asset.name != "moyai-sample-numbers.csv"
            || asset.sha256 != sha256
            || asset.byte_length != bytes.len() as u64
            || asset.kind != "input"
            || asset.purged_at_ms.is_some()
        {
            return Err(RequestError::Invalid);
        }
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        let mut runtime = self.inner.shared_work.0.lock().unwrap();
        runtime.require_project(project_id, generation, query)?;
        if !runtime.view.can_prepare_sample(project_id) {
            return Err(RequestError::Invalid);
        }
        runtime.view.attach_input(asset)?;
        runtime.view.feedback =
            Some("サンプルを添付しました。依頼内容と実行PCを確認して送信してください。".into());
        Ok(())
    }
    pub(super) async fn shared_upload_inputs(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        project_id: &str,
    ) -> Result<(), RequestError> {
        {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_project(project_id, generation, query)?;
            if !runtime
                .view
                .projects
                .iter()
                .any(|p| p.id == project_id && p.can_submit)
            {
                return Err(RequestError::Http(403));
            }
        }
        let paths = pick_inputs().await?;
        for path in paths {
            if !self.shared_current(connection, generation, query) {
                return Ok(());
            }
            let (name, bytes, sha256) = tokio::task::spawn_blocking(move || {
                let path = camino::Utf8PathBuf::from_path_buf(path)
                    .map_err(|_| RequestError::Local("ファイル名をUTF-8で読み取れません。"))?;
                let name = path.file_name().ok_or(RequestError::Invalid)?.to_owned();
                let (bytes, identity) = crate::edit::read_file_with_identity(&path, MAX_FILE)
                    .map_err(|e| RequestError::Filesystem(e.to_string()))?;
                Ok::<_, RequestError>((name, bytes, identity.content_sha256))
            })
            .await
            .map_err(|_| RequestError::Unavailable)??;
            {
                let runtime = self.inner.shared_work.0.lock().unwrap();
                runtime.require_project(project_id, generation, query)?;
                runtime.view.input_slot(&name)?;
            }
            // Selecting a file is a new upload intent, including reattachment after retention.
            // An unreferenced upload is independent from the durable job submission receipt.
            let request_id = format!("asset-{}", ulid::Ulid::new());
            let asset: WorkAsset = request(&connection.client, &format!("projects/{project_id}/assets"), Some(token), Some(json!({"request_id":request_id,"name":name,"sha256":sha256,"content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})), &[]).await?;
            if asset.project_id != project_id
                || asset.sha256 != sha256
                || asset.kind != "input"
                || asset.purged_at_ms.is_some()
            {
                return Err(RequestError::Invalid);
            }
            if !self.shared_current(connection, generation, query) {
                return Ok(());
            }
            let mut runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_project(project_id, generation, query)?;
            runtime.view.attach_input(asset)?;
        }
        Ok(())
    }
    pub(super) async fn shared_save_asset(
        &self,
        connection: &Connection,
        generation: u64,
        query: u64,
        token: &str,
        project_id: &str,
        job_id: &str,
        asset_id: &str,
        import: bool,
    ) -> Result<(), RequestError> {
        let asset = {
            let runtime = self.inner.shared_work.0.lock().unwrap();
            runtime.require_project(project_id, generation, query)?;
            if runtime.view.selected_job_id.as_deref() != Some(job_id) {
                return Err(RequestError::Invalid);
            }
            runtime
                .view
                .assets
                .iter()
                .find(|a| {
                    a.id == asset_id && a.project_id == project_id && a.purged_at_ms.is_none()
                })
                .cloned()
                .ok_or(RequestError::Invalid)?
        };
        let response: Download = request(
            &connection.client,
            &format!("assets/{asset_id}"),
            Some(token),
            None,
            &[],
        )
        .await?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(response.content_base64)
            .map_err(|_| RequestError::Invalid)?;
        if response.asset.id != asset.id
            || response.asset.sha256 != asset.sha256
            || bytes.len() as u64 != asset.byte_length
            || bytes.len() as u64 > MAX_FILE
            || format!("{:x}", Sha256::digest(&bytes)) != asset.sha256
        {
            return Err(RequestError::Invalid);
        }
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        let name = std::path::Path::new(&asset.name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(RequestError::Invalid)?
            .to_owned();
        let Some(destination) = pick_destination(name, import).await? else {
            return Ok(());
        };
        if !self.shared_current(connection, generation, query) {
            return Ok(());
        }
        let binding = connection.binding.clone();
        let service = self.clone();
        let project_id = project_id.to_owned();
        tokio::task::spawn_blocking(move || {
            if service
                .shared_connection()
                .is_none_or(|c| c.binding != binding)
            {
                return Ok(());
            }
            let mut runtime = service.inner.shared_work.0.lock().unwrap();
            runtime.require_project(&project_id, generation, query)?;
            let destination = camino::Utf8PathBuf::from_path_buf(destination)
                .map_err(|_| RequestError::Local("保存先をUTF-8で読み取れません。"))?;
            save_asset(
                &destination,
                &bytes,
                if import {
                    asset.base_sha256.as_deref()
                } else {
                    None
                },
                import,
            )?;
            runtime.view.feedback = Some(format!("保存しました: {destination}"));
            Ok::<_, RequestError>(())
        })
        .await
        .map_err(|_| RequestError::Unavailable)??;
        Ok(())
    }
}
fn save_asset(
    destination: &camino::Utf8Path,
    bytes: &[u8],
    base: Option<&str>,
    import: bool,
) -> Result<(), RequestError> {
    let parent = destination.parent().ok_or(RequestError::Invalid)?;
    let guarded = crate::workspace::PathGuard::trusted_internal_path(destination, parent)
        .map_err(|e| RequestError::Filesystem(e.to_string()))?;
    let expected = if destination
        .try_exists()
        .map_err(|e| RequestError::Filesystem(e.to_string()))?
    {
        let (_, identity) = crate::edit::read_file_with_identity(destination, MAX_FILE)
            .map_err(|e| RequestError::Filesystem(e.to_string()))?;
        if import && base != Some(identity.content_sha256.as_str()) {
            return Err(RequestError::Local(
                "取り込み先が元の版と一致しません。既存ファイルを保持しました。別名で保存して差分を確認してください。",
            ));
        }
        Some(identity)
    } else {
        None
    };
    crate::tool::write_support::write_bytes_file_conditionally(
        &guarded,
        bytes,
        expected.as_ref(),
        |_| Ok(()),
    )
    .map_err(|e| RequestError::Filesystem(e.to_string()))?;
    Ok(())
}
#[cfg(feature = "tauri-desktop")]
async fn pick_inputs() -> Result<Vec<std::path::PathBuf>, RequestError> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("共有仕事へ渡す入力ファイルを選択")
            .pick_files()
            .unwrap_or_default()
    })
    .await
    .map_err(|_| RequestError::Unavailable)
}
#[cfg(not(feature = "tauri-desktop"))]
async fn pick_inputs() -> Result<Vec<std::path::PathBuf>, RequestError> {
    Err(RequestError::Local(
        "ファイルの選択はDesktopで操作してください。",
    ))
}
#[cfg(feature = "tauri-desktop")]
async fn pick_destination(
    name: String,
    import: bool,
) -> Result<Option<std::path::PathBuf>, RequestError> {
    tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(if import {
                "成果の取り込み先を選択（元の版と照合）"
            } else {
                "共有ファイルを保存"
            })
            .set_file_name(name)
            .save_file()
    })
    .await
    .map_err(|_| RequestError::Unavailable)
}
#[cfg(not(feature = "tauri-desktop"))]
async fn pick_destination(_: String, _: bool) -> Result<Option<std::path::PathBuf>, RequestError> {
    Err(RequestError::Local(
        "ファイルの保存はDesktopで操作してください。",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_is_only_prepared_for_an_empty_authorized_new_conversation() {
        let mut view = SharedWorkProjection::default();
        view.projects.push(WorkProject {
            id: "p".into(),
            label: "P".into(),
            role: "contributor".into(),
            can_submit: true,
        });
        assert!(view.can_prepare_sample("p"));
        assert!(!view.can_prepare_sample("another-project"));
        view.projects[0].can_submit = false;
        assert!(!view.can_prepare_sample("p"));
        view.projects[0].can_submit = true;
        view.submission_uncertain = true;
        assert!(!view.can_prepare_sample("p"));
        view.submission_uncertain = false;
        view.selected_job_id = Some("existing-job".into());
        assert!(!view.can_prepare_sample("p"));
        view.selected_job_id = None;
        view.submission_storage_error = Some("unreadable receipt".into());
        assert!(!view.can_prepare_sample("p"));
    }
    #[test]
    fn shared_feedback_survives_background_refresh_but_not_user_or_identity_changes() {
        let mut runtime = Runtime::default();
        runtime.token = Some("alice-session".into());
        runtime.view.feedback = Some("同名の添付を置き換えました".into());
        for _ in 0..3 {
            let (generation, query, _) = runtime.begin_command(&SharedWorkCommand::Refresh);
            runtime.require_current(generation, query).unwrap();
            assert!(runtime.projection(true, "hub".into()).feedback.is_some());
        }
        runtime.begin_command(&SharedWorkCommand::Project {
            project_id: "next".into(),
        });
        assert!(runtime.view.feedback.is_none());
        runtime.view.feedback = Some("Aliceの保存結果".into());
        let (old_generation, old_query, token) = runtime.begin_command(&SharedWorkCommand::Logout);
        assert_eq!(token.as_deref(), Some("alice-session"));
        assert!(runtime.view.feedback.is_none());
        assert!(runtime.require_current(old_generation, old_query).is_err());
        runtime.view.feedback = Some("前の接続の結果".into());
        runtime.clear(); // The same owner handles 401/403, expiry and changed Hub identity.
        assert!(runtime.view.feedback.is_none());
    }
    #[test]
    fn input_reselection_replaces_same_name_at_capacity_without_removing_other_refs() {
        let asset = |id: &str, name: &str| WorkAsset {
            id: id.into(),
            project_id: "project".into(),
            job_id: None,
            kind: "input".into(),
            name: name.into(),
            sha256: "a".repeat(64),
            byte_length: 1,
            created_at_ms: 1,
            version: 1,
            base_sha256: None,
            purged_at_ms: None,
        };
        let mut view = SharedWorkProjection::default();
        view.attach_input(asset("old", "Ä-input.txt")).unwrap();
        for index in 1..32 {
            view.attach_input(asset(
                &format!("old-{index}"),
                &format!("input-{index}.txt"),
            ))
            .unwrap();
        }
        let others: Vec<_> = view.inputs[1..]
            .iter()
            .map(|item| item.id.clone())
            .collect();
        view.attach_input(asset("fresh-upload", "ä-INPUT.TXT"))
            .unwrap();
        assert_eq!(view.inputs.len(), 32);
        assert_eq!(view.inputs[0].id, "fresh-upload");
        assert_eq!(
            view.inputs[1..]
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            others
        );
        assert!(view.feedback.as_ref().unwrap().contains("置き換えました"));
        assert!(view.attach_input(asset("extra", "extra.txt")).is_err());
        assert_eq!(view.inputs.len(), 32);
    }
    #[test]
    fn shared_binary_save_and_import_preserve_changed_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(temp.path().join("result.bin")).unwrap();
        let original = [0, 255, 128, 10];
        save_asset(&path, &original, None, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let base = format!("{:x}", Sha256::digest(original));
        assert!(save_asset(&path, b"new", None, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        save_asset(&path, b"accepted", Some(&base), true).unwrap();
        assert!(save_asset(&path, b"overwrite", Some(&base), true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"accepted");
    }
    #[cfg(windows)]
    #[test]
    fn shared_binary_tree_is_new_and_keeps_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(temp.path().join("inputs")).unwrap();
        let guarded =
            crate::workspace::PathGuard::trusted_internal_path(&path, path.parent().unwrap())
                .unwrap();
        crate::tool::write_support::create_new_bytes_tree(
            &guarded,
            &[("nested/input.bin".into(), vec![0, 255, 127])],
        )
        .unwrap();
        assert_eq!(
            std::fs::read(path.join("nested/input.bin")).unwrap(),
            [0, 255, 127]
        );
        assert!(crate::tool::write_support::create_new_bytes_tree(&guarded, &[]).is_err());
    }
}
