use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Owns the setup launch result until the first ready or error notification.
#[derive(Default)]
pub struct StartupReport {
    pending: Mutex<Option<File>>,
}

#[derive(serde::Serialize)]
struct StartupResult<'a> {
    schema: u8,
    status: &'static str,
    process_id: u32,
    exit_code: u8,
    message: &'a str,
}

impl StartupReport {
    pub fn create(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                format!(
                    "起動結果の記録を作成できません（{}）: {error}",
                    path.display()
                )
            })?;
        Ok(Self {
            pending: Mutex::new(Some(file)),
        })
    }

    pub fn ready(&self) -> Result<bool, String> {
        self.finish("ready", 0, "moyAIの初期画面を表示しました。")
    }

    pub fn error(&self, exit_code: u8, message: &str) -> Result<bool, String> {
        self.finish("error", exit_code, message)
    }

    fn finish(&self, status: &'static str, exit_code: u8, message: &str) -> Result<bool, String> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(mut file) = pending.take() else {
            return Ok(false);
        };
        let result = StartupResult {
            schema: 1,
            status,
            process_id: std::process::id(),
            exit_code,
            message,
        };
        serde_json::to_writer(&mut file, &result)
            .map_err(|error| format!("起動結果の記録を書き込めません: {error}"))?;
        file.flush()
            .map_err(|error| format!("起動結果の記録を保存できません: {error}"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::StartupReport;

    #[test]
    fn ready_closes_one_result_and_later_errors_do_not_replace_it() {
        let directory = tempfile::tempdir().expect("temporary report directory");
        let path = directory.path().join("startup.json");
        let report = StartupReport::create(&path).expect("new startup report");
        assert!(std::fs::read(&path).expect("pending report").is_empty());
        assert!(report.ready().expect("ready report"));
        assert!(!report.error(4, "later failure").expect("already complete"));
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("ready report file"))
                .expect("one JSON result");
        assert_eq!(result["schema"], 1);
        assert_eq!(result["status"], "ready");
        assert_eq!(result["process_id"], std::process::id());
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["message"], "moyAIの初期画面を表示しました。");
        std::fs::rename(&path, directory.path().join("closed.json"))
            .expect("result file is closed");
    }

    #[test]
    fn error_preserves_the_actual_message_and_exit_code() {
        let directory = tempfile::tempdir().expect("temporary report directory");
        let path = directory.path().join("startup.json");
        let report = StartupReport::create(&path).expect("new startup report");
        let message = "tauri desktop runtime failed: 画面を作成できません";
        assert!(report.error(4, message).expect("error report"));
        assert!(!report.ready().expect("already failed"));
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("error report file"))
                .expect("one JSON result");
        assert_eq!(result["status"], "error");
        assert_eq!(result["exit_code"], 4);
        assert_eq!(result["message"], message);
    }

    #[test]
    fn existing_report_is_never_overwritten() {
        let directory = tempfile::tempdir().expect("temporary report directory");
        let path = directory.path().join("startup.json");
        std::fs::write(&path, "previous result").expect("existing report");
        assert!(StartupReport::create(&path).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "previous result");
    }

    #[test]
    fn concurrent_ready_and_error_leave_one_complete_result() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().expect("temporary report directory");
        let path = directory.path().join("startup.json");
        let report = Arc::new(StartupReport::create(&path).expect("new startup report"));
        let barrier = Arc::new(Barrier::new(2));
        let ready_report = report.clone();
        let ready_barrier = barrier.clone();
        let ready = std::thread::spawn(move || {
            ready_barrier.wait();
            ready_report.ready().expect("ready notification")
        });
        let error = std::thread::spawn(move || {
            barrier.wait();
            report
                .error(3, "startup failed")
                .expect("error notification")
        });
        let ready_won = ready.join().expect("ready thread");
        let error_won = error.join().expect("error thread");
        assert_ne!(ready_won, error_won);
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("completed report"))
                .expect("one complete JSON result");
        assert_eq!(result["status"], if ready_won { "ready" } else { "error" });
        assert_eq!(result["exit_code"], if ready_won { 0 } else { 3 });
    }

    #[test]
    fn ordinary_launch_has_no_report_side_effect() {
        let report = StartupReport::default();
        assert!(!report.ready().expect("disabled ready notification"));
        assert!(
            !report
                .error(3, "disabled")
                .expect("disabled error notification")
        );
    }
}
