use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentTimeSnapshot {
    pub utc: String,
    pub local: String,
    pub timezone: String,
    pub unix_ms: i64,
}

impl CurrentTimeSnapshot {
    pub fn now() -> Self {
        let utc: DateTime<Utc> = Utc::now();
        let local: DateTime<Local> = utc.with_timezone(&Local);
        Self {
            utc: utc.to_rfc3339_opts(SecondsFormat::Secs, true),
            local: local.to_rfc3339_opts(SecondsFormat::Secs, false),
            timezone: local.offset().to_string(),
            unix_ms: utc.timestamp_millis(),
        }
    }

    pub fn reminder_text(&self) -> String {
        format!("Current time: {} local (UTC {}).", self.local, self.utc)
    }
}

pub fn current_time_reminder() -> String {
    CurrentTimeSnapshot::now().reminder_text()
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_time_snapshot_renders_utc_and_local() {
        let snapshot = super::CurrentTimeSnapshot::now();

        assert!(snapshot.utc.ends_with('Z'));
        assert!(snapshot.local.contains('T'));
        assert!(snapshot.unix_ms > 0);
        assert!(snapshot.reminder_text().contains("Current time:"));
    }
}
