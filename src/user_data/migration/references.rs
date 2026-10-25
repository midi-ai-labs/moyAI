//! Relocate typed local path metadata, never canonical conversation text.
use std::fs;
use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::runtime::resource_admission::relocated;

pub(super) struct Undo {
    files: Vec<(Utf8PathBuf, Vec<u8>)>,
    cells: Vec<(&'static str, &'static str, i64, String)>,
    sidecars: Vec<(String, String)>,
}

#[derive(Debug)]
pub(super) enum RelocationFailure {
    Restored { cause: String },
    RecoveryRequired { cause: String, rollback: String },
}

impl std::fmt::Display for RelocationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Restored { cause } => formatter.write_str(cause),
            Self::RecoveryRequired { cause, rollback } => write!(
                formatter,
                "参照更新に失敗し、元の参照の復元も完了できませんでした: {cause}; {rollback}"
            ),
        }
    }
}

pub(super) fn relocate(
    stage: &Utf8Path,
    final_root: &Utf8Path,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
) -> Result<Undo, RelocationFailure> {
    relocate_with_writer(stage, final_root, mappings, write_atomic)
}

pub(super) fn relocate_with_writer(
    stage: &Utf8Path,
    final_root: &Utf8Path,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
    mut write: impl FnMut(&Utf8Path, &[u8]) -> Result<(), String>,
) -> Result<Undo, RelocationFailure> {
    let mut undo = Undo {
        files: Vec::new(),
        cells: Vec::new(),
        sidecars: Vec::new(),
    };
    let result = (|| {
        let mut pending = vec![stage.join("config")];
        while let Some(path) = pending.pop() {
            if !path.exists() {
                continue;
            }
            if path.is_dir() {
                for entry in fs::read_dir(&path).map_err(|error| error.to_string())? {
                    pending.push(
                        Utf8PathBuf::from_path_buf(
                            entry.map_err(|error| error.to_string())?.path(),
                        )
                        .map_err(|_| "Config path is not UTF-8")?,
                    );
                }
            } else {
                relocate_document(stage, &path, mappings, &mut undo, &mut write)?;
            }
        }
        relocate_document(
            stage,
            &stage.join("data/runner-operations.json"),
            mappings,
            &mut undo,
            &mut write,
        )?;
        let database = stage.join("data/moyai.sqlite3");
        if database.exists() {
            relocate_database(&database, stage, final_root, mappings, &mut undo)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(undo),
        Err(error) => {
            if let Err(rollback) = restore_with_writer(stage, undo, &mut write) {
                return Err(RelocationFailure::RecoveryRequired {
                    cause: error,
                    rollback,
                });
            }
            Err(RelocationFailure::Restored { cause: error })
        }
    }
}

fn relocate_document(
    stage: &Utf8Path,
    path: &Utf8Path,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
    undo: &mut Undo,
    write: &mut impl FnMut(&Utf8Path, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if !path.exists() || !matches!(path.extension(), Some("json" | "toml")) {
        return Ok(());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err(format!("設定ファイルが読込上限を超えています: {path}"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let updated = if path.extension() == Some("json") {
        let mut value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| format!("{path}: {error}"))?;
        if !relocate_json(&mut value, "", mappings) {
            return Ok(());
        }
        serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?
    } else {
        let mut value: toml::Value =
            toml::from_str(text).map_err(|error| format!("{path}: {error}"))?;
        if !relocate_toml(&mut value, "", mappings) {
            return Ok(());
        }
        toml::to_string_pretty(&value)
            .map_err(|error| error.to_string())?
            .into_bytes()
    };
    undo.files.push((
        path.strip_prefix(stage)
            .map_err(|error| error.to_string())?
            .to_owned(),
        bytes,
    ));
    write(path, &updated)
}

fn path_field(key: &str) -> bool {
    matches!(
        key,
        "certificate_path"
            | "private_key_path"
            | "trusted_certificate_path"
            | "workspace_root"
            | "directory"
            | "expected_directory"
            | "last_workspace"
            | "deleted_project_roots"
            | "additional_read_roots"
            | "additional_write_roots"
            | "additional_files"
            | "protected_paths"
            | "config_path"
            | "data_dir"
            | "program"
    )
}

fn relocate_string(text: &mut String, key: &str, mappings: &[(Utf8PathBuf, Utf8PathBuf)]) -> bool {
    if !path_field(key) {
        return false;
    }
    let next = relocated(Utf8Path::new(text), mappings).into_string();
    if next == *text {
        return false;
    }
    *text = next;
    true
}

fn relocate_json(
    value: &mut serde_json::Value,
    key: &str,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
) -> bool {
    match value {
        serde_json::Value::String(text) => relocate_string(text, key, mappings),
        serde_json::Value::Array(values) => values.iter_mut().fold(false, |changed, value| {
            relocate_json(value, key, mappings) || changed
        }),
        serde_json::Value::Object(values) => {
            values.iter_mut().fold(false, |changed, (key, value)| {
                relocate_json(value, key, mappings) || changed
            })
        }
        _ => false,
    }
}

fn relocate_toml(
    value: &mut toml::Value,
    key: &str,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
) -> bool {
    match value {
        toml::Value::String(text) => relocate_string(text, key, mappings),
        toml::Value::Array(values) => values.iter_mut().fold(false, |changed, value| {
            relocate_toml(value, key, mappings) || changed
        }),
        toml::Value::Table(values) => values.iter_mut().fold(false, |changed, (key, value)| {
            relocate_toml(value, key, mappings) || changed
        }),
        _ => false,
    }
}

fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut statement = db
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| error.to_string())?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(names.iter().any(|name| name == column))
}

fn relocate_database(
    database: &Utf8Path,
    stage: &Utf8Path,
    final_root: &Utf8Path,
    mappings: &[(Utf8PathBuf, Utf8PathBuf)],
    undo: &mut Undo,
) -> Result<(), String> {
    let mut db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| error.to_string())?;
    let transaction = db.transaction().map_err(|error| error.to_string())?;
    for (table, column) in [
        ("projects", "root_path"),
        ("sessions", "cwd_path"),
        ("file_changes", "path_before"),
        ("file_changes", "path_after"),
        ("shared_history_files", "local_path"),
        ("harness_contracts", "source_path"),
    ] {
        if !has_column(&transaction, table, column)? {
            continue;
        }
        let rows = {
            let mut statement = transaction
                .prepare(&format!(
                    "SELECT rowid,{column} FROM {table} WHERE {column} IS NOT NULL"
                ))
                .map_err(|error| error.to_string())?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (rowid, before) in rows {
            let after = relocated(Utf8Path::new(&before), mappings);
            if after.as_str() != before {
                transaction
                    .execute(
                        &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                        params![after.as_str(), rowid],
                    )
                    .map_err(|error| error.to_string())?;
                undo.cells.push((table, column, rowid, before));
            }
        }
    }
    if has_column(&transaction, "tool_calls", "truncated_output_path")?
        && has_column(&transaction, "protocol_history_items", "session_id")?
    {
        let rows = {
            let mut statement = transaction.prepare("SELECT DISTINCT h.session_id,t.truncated_output_path FROM tool_calls t JOIN protocol_history_items h ON h.id=t.history_item_id WHERE t.truncated_output_path IS NOT NULL").map_err(|error| error.to_string())?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (session, original) in rows {
            let local = relocated(Utf8Path::new(&original), mappings);
            if local.as_str() == original {
                continue;
            }
            let Ok(relative) = local.strip_prefix(final_root.join("data/truncation")) else {
                continue;
            };
            let physical = stage.join("data/truncation").join(relative);
            if !physical.is_file() {
                continue;
            }
            if !has_column(&transaction, "shared_history_files", "local_path")? {
                return Err("旧会話の出力参照を安全に移行できない保存形式です。旧保存先を保持して中止しました。".into());
            }
            let existing: Option<String> = transaction.query_row("SELECT local_path FROM shared_history_files WHERE session_id=?1 AND original_path=?2", params![session,original], |row| row.get(0)).optional().map_err(|error| error.to_string())?;
            if existing.is_some() {
                continue;
            }
            let mut file = fs::File::open(&physical).map_err(|error| error.to_string())?;
            let mut digest = Sha256::new();
            let mut buffer = [0; 64 * 1024];
            loop {
                let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            transaction.execute("INSERT INTO shared_history_files(session_id,original_path,local_path,sha256) VALUES(?1,?2,?3,?4)",params![session,original,local.as_str(),format!("{:x}",digest.finalize())]).map_err(|error| error.to_string())?;
            undo.sidecars.push((session, original));
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn restore(root: &Utf8Path, undo: Undo) -> Result<(), String> {
    restore_with_writer(root, undo, &mut write_atomic)
}

fn restore_with_writer(
    root: &Utf8Path,
    undo: Undo,
    write: &mut impl FnMut(&Utf8Path, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if !undo.cells.is_empty() || !undo.sidecars.is_empty() {
        let mut db = Connection::open_with_flags(
            root.join("data/moyai.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|error| error.to_string())?;
        let transaction = db.transaction().map_err(|error| error.to_string())?;
        for (table, column, rowid, value) in undo.cells.into_iter().rev() {
            transaction
                .execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                    params![value, rowid],
                )
                .map_err(|error| error.to_string())?;
        }
        for (session, original) in undo.sidecars {
            transaction
                .execute(
                    "DELETE FROM shared_history_files WHERE session_id=?1 AND original_path=?2",
                    params![session, original],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(|error| error.to_string())?;
    }
    for (relative, bytes) in undo.files.into_iter().rev() {
        write(&root.join(relative), &bytes)?;
    }
    Ok(())
}

pub(super) fn write_atomic(path: &Utf8Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Saved file has no parent")?)
            .map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_paths_preserve_external_files_and_canonical_text_then_rollback() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let stage = root.join("stage");
        let destination = root.join("new/desktop");
        let old = root.join("old/moyai");
        fs::create_dir_all(stage.join("config")).unwrap();
        fs::create_dir_all(stage.join("data/truncation")).unwrap();
        let original = old.join("data/truncation/saved.txt");
        fs::write(stage.join("data/truncation/saved.txt"), "output").unwrap();
        let document = serde_json::json!({"tls":{"certificate_path":old.join("config/cert.pem"),"private_key_path":root.join("external/key.pem")},"note":old.join("config/cert.pem")});
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(stage.join("config/mcp-publish.json"), &bytes).unwrap();
        let shell = format!(
            "[shell]\nprogram = '{}'\n[instructions]\nadditional_files = ['{}']\n",
            old.join("config/bin/shell.exe"),
            root.join("external/instructions.md")
        );
        fs::write(stage.join("config/config.toml"), &shell).unwrap();
        let db = Connection::open(stage.join("data/moyai.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE projects(root_path TEXT); CREATE TABLE sessions(cwd_path TEXT); CREATE TABLE protocol_history_items(id TEXT,session_id TEXT,body TEXT); CREATE TABLE tool_calls(history_item_id TEXT,truncated_output_path TEXT); CREATE TABLE shared_history_files(session_id TEXT,original_path TEXT,local_path TEXT,sha256 TEXT,PRIMARY KEY(session_id,original_path));").unwrap();
        db.execute(
            "INSERT INTO projects VALUES(?1)",
            [old.join("data/quick-chat-workspace").as_str()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO projects VALUES(?1)",
            [root.join("external/workspace").as_str()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO protocol_history_items VALUES('h','s',?1)",
            [format!("read {original}")],
        )
        .unwrap();
        db.execute("INSERT INTO tool_calls VALUES('h',?1)", [original.as_str()])
            .unwrap();
        drop(db);
        let mappings = [
            (old.join("config"), destination.join("config")),
            (old.join("data"), destination.join("data")),
        ];
        let undo = relocate(&stage, &destination, &mappings).unwrap();
        let updated: serde_json::Value =
            serde_json::from_slice(&fs::read(stage.join("config/mcp-publish.json")).unwrap())
                .unwrap();
        assert_eq!(
            Utf8Path::new(updated["tls"]["certificate_path"].as_str().unwrap()),
            destination.join("config/cert.pem")
        );
        assert_eq!(
            updated["tls"]["private_key_path"],
            document["tls"]["private_key_path"]
        );
        assert_eq!(updated["note"], document["note"]);
        let updated_shell: toml::Value =
            toml::from_str(&fs::read_to_string(stage.join("config/config.toml")).unwrap()).unwrap();
        assert_eq!(
            Utf8Path::new(updated_shell["shell"]["program"].as_str().unwrap()),
            destination.join("config/bin/shell.exe")
        );
        assert_eq!(
            Utf8Path::new(
                updated_shell["instructions"]["additional_files"][0]
                    .as_str()
                    .unwrap()
            ),
            root.join("external/instructions.md")
        );
        let db = Connection::open(stage.join("data/moyai.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT body FROM protocol_history_items", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
            format!("read {original}")
        );
        assert_eq!(
            Utf8PathBuf::from(
                db.query_row("SELECT local_path FROM shared_history_files", [], |row| row
                    .get::<_, String>(0))
                    .unwrap()
            ),
            destination.join("data/truncation/saved.txt")
        );
        assert_eq!(
            db.query_row(
                "SELECT root_path FROM projects ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            root.join("external/workspace").as_str()
        );
        drop(db);
        restore(&stage, undo).unwrap();
        assert_eq!(
            fs::read(stage.join("config/mcp-publish.json")).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read_to_string(stage.join("config/config.toml")).unwrap(),
            shell
        );
        let db = Connection::open(stage.join("data/moyai.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM shared_history_files", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT root_path FROM projects ORDER BY rowid LIMIT 1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            old.join("data/quick-chat-workspace").as_str()
        );
    }
}
