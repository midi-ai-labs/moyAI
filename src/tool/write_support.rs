use std::io::Write;

use camino::Utf8Path;
use tempfile::NamedTempFile;

use crate::edit::{
    ChangeSummary, FileChange, FileContentIdentity, FileReadStamp, read_file_with_identity,
};
use crate::error::EditError;
use crate::runtime::SystemClock;

pub(crate) fn write_text_file(path: &Utf8Path, text: &str) -> Result<(), EditError> {
    let parent = path
        .parent()
        .ok_or_else(|| EditError::Message("file path has no parent".to_string()))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(text.as_bytes())?;
    temp.flush()?;
    temp.persist(path)
        .map_err(|error| EditError::Io(error.error))?;
    Ok(())
}

pub(crate) fn write_text_file_noclobber(path: &Utf8Path, text: &str) -> Result<(), EditError> {
    let parent = path
        .parent()
        .ok_or_else(|| EditError::Message("file path has no parent".to_string()))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(text.as_bytes())?;
    temp.flush()?;
    temp.persist_noclobber(path)
        .map_err(|error| EditError::Message(format!(
            "path `{path}` was created before the new file could be committed; the existing file was not overwritten: {}",
            error.error
        )))?;
    Ok(())
}

pub(crate) fn read_text_file_with_identity(
    path: &Utf8Path,
) -> Result<(String, FileContentIdentity), EditError> {
    let (bytes, identity) = read_file_with_identity(path)?;
    let text = String::from_utf8(bytes).map_err(|error| {
        EditError::Message(format!("path `{path}` is not valid UTF-8 text: {error}"))
    })?;
    Ok((text, identity))
}

pub(crate) fn build_read_stamp(path: &Utf8Path) -> Result<FileReadStamp, EditError> {
    let (_, identity) = read_file_with_identity(path)?;
    Ok(FileReadStamp {
        path: path.to_path_buf(),
        read_at_ms: SystemClock::now_ms(),
        mtime_ms: identity.mtime_ms,
        size_bytes: Some(identity.size_bytes),
        content_sha256: Some(identity.content_sha256),
    })
}

pub(crate) fn to_summary(change: &FileChange) -> ChangeSummary {
    ChangeSummary {
        change_id: change.id,
        kind: change.kind,
        path_before: change.path_before.clone(),
        path_after: change.path_after.clone(),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    #[test]
    fn no_clobber_create_preserves_external_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");
        std::fs::write(&path, "external").expect("seed external file");

        let error = super::write_text_file_noclobber(&path, "agent")
            .expect_err("no-clobber write must reject an existing file");

        assert!(error.to_string().contains("was not overwritten"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }
}
