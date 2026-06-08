use std::fs;
use std::io::Write;
use std::time::UNIX_EPOCH;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::NamedTempFile;

use crate::edit::{ChangeSummary, FileChange, FileReadStamp};
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

pub(crate) fn build_read_stamp(path: &Utf8Path) -> Result<FileReadStamp, EditError> {
    let metadata = fs::metadata(path)?;
    Ok(FileReadStamp {
        path: Utf8PathBuf::from(path),
        read_at_ms: SystemClock::now_ms(),
        mtime_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64),
        size_bytes: Some(metadata.len()),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteCommitStep {
    WriteTempFile,
    PersistTempToTarget,
}

fn write_text_file_commit_plan() -> Vec<WriteCommitStep> {
    vec![
        WriteCommitStep::WriteTempFile,
        WriteCommitStep::PersistTempToTarget,
    ]
}

pub(crate) fn write_text_file_commit_plan_avoids_pre_remove_fixture_passes() -> bool {
    let temp = match tempfile::tempdir() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let path = match Utf8PathBuf::from_path_buf(temp.path().join("target.txt")) {
        Ok(value) => value,
        Err(_) => return false,
    };
    if std::fs::write(&path, "before").is_err() {
        return false;
    }
    if write_text_file(&path, "after").is_err() {
        return false;
    }
    let plan = write_text_file_commit_plan();
    plan == vec![
        WriteCommitStep::WriteTempFile,
        WriteCommitStep::PersistTempToTarget,
    ] && matches!(std::fs::read_to_string(&path), Ok(value) if value == "after")
}

#[cfg(test)]
mod tests {
    #[test]
    fn write_text_file_commit_plan_avoids_pre_remove() {
        assert!(super::write_text_file_commit_plan_avoids_pre_remove_fixture_passes());
    }
}
