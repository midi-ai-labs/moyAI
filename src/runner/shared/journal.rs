//! Local evidence of possible execution. Hub owns scheduling; this journal never retries effects.
use std::fs::{File, OpenOptions};

use camino::Utf8Path;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::super::RunnerError;
use super::{Assignment, EnvironmentMapping, Report, ReportOutcome, SharedSettings};

pub(crate) struct Journal {
    db: Connection,
    _lock: File,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Intent,
    Executing,
    ReportPending,
    Uncertain,
    Settled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub assignment: Assignment,
    pub mapping: EnvironmentMapping,
    pub run_id: Ulid,
    pub phase: Phase,
    pub report: Option<Report>,
    #[serde(default)]
    pub approval_report: Option<Report>,
    #[serde(default)]
    pub fallback_report: Option<Report>,
    /// Exact local cleanup receipt, independent of whether the Hub accepted a child handoff.
    #[serde(default)]
    pub local_checkpoint: Option<serde_json::Value>,
    /// Delivery acknowledgement only; canonical checkpoint state remains in SessionRepository.
    #[serde(default)]
    pub checkpoint_settlement_ack: bool,
    #[serde(default)]
    pub external: Option<super::external::ExternalEvidence>,
}

impl Entry {
    pub(crate) fn checkpoint(&self) -> Option<&serde_json::Value> {
        self.local_checkpoint.as_ref().or_else(|| {
            // Existing task journals predate the common local receipt field.
            match &self.report.as_ref()?.outcome {
                ReportOutcome::YieldToChild { checkpoint, .. } => Some(checkpoint),
                _ => None,
            }
        })
    }
}

impl Journal {
    pub(crate) fn open(path: &Utf8Path, settings: &SharedSettings) -> Result<Self, RunnerError> {
        let parent = path
            .parent()
            .ok_or_else(|| RunnerError::new("Invalid Runner journal directory"))?;
        std::fs::create_dir_all(parent).map_err(error)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .map_err(error)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
            RunnerError::new("This shared Runner journal is already owned by another process")
        })?;
        let mut db = Connection::open(path).map_err(error)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(error)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(error)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(error)?;
        if version > 1 {
            return Err(RunnerError::new(
                "This Runner journal needs a newer application version",
            ));
        }
        if version == 0 {
            let tx = db.transaction().map_err(error)?;
            tx.execute_batch("CREATE TABLE runner_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),hub_id TEXT NOT NULL,device_id TEXT NOT NULL);
                CREATE TABLE runner_attempts(attempt_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,entry_json TEXT NOT NULL);
                PRAGMA user_version=1;").map_err(error)?;
            tx.execute(
                "INSERT INTO runner_identity VALUES(1,?1,?2)",
                params![settings.hub_id, settings.device_id],
            )
            .map_err(error)?;
            tx.commit().map_err(error)?;
        }
        let identity: (String, String) = db
            .query_row(
                "SELECT hub_id,device_id FROM runner_identity WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(error)?;
        if identity != (settings.hub_id.clone(), settings.device_id.clone()) {
            return Err(RunnerError::new(
                "Runner journal belongs to a different Hub/device; it cannot be reused",
            ));
        }
        Ok(Self { db, _lock: lock })
    }

    pub(crate) fn get(&self, attempt: &str) -> Result<Option<Entry>, RunnerError> {
        let value: Option<String> = self
            .db
            .query_row(
                "SELECT entry_json FROM runner_attempts WHERE attempt_id=?1",
                [attempt],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(error))
            .transpose()
    }

    pub(crate) fn active(&self) -> Result<Vec<Entry>, RunnerError> {
        let mut stmt = self.db.prepare("SELECT entry_json FROM runner_attempts WHERE json_extract(entry_json,'$.phase') != 'settled' ORDER BY attempt_id LIMIT 129").map_err(error)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(serde_json::from_str(&row.map_err(error)?).map_err(error)?);
        }
        if result.len() > 128 {
            return Err(RunnerError::new(
                "Runner journal active-attempt limit exceeded; reconciliation is required",
            ));
        }
        Ok(result)
    }

    pub(crate) fn unsettled_checkpoints(&self, after: &str) -> Result<Vec<Entry>, RunnerError> {
        let mut statement = self
            .db
            .prepare(
                "SELECT entry_json FROM runner_attempts
             WHERE attempt_id > ?1 AND json_extract(entry_json,'$.phase') = 'settled'
               AND COALESCE(json_extract(entry_json,'$.local_checkpoint'),json_extract(entry_json,'$.report.outcome.checkpoint')) IS NOT NULL
               AND COALESCE(json_extract(entry_json,'$.checkpoint_settlement_ack'),0) = 0
             ORDER BY attempt_id LIMIT 1",
            )
            .map_err(error)?;
        let rows = statement
            .query_map([after], |row| row.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
            .collect()
    }

    pub(crate) fn acknowledge_checkpoint_settlement(
        &mut self,
        entry: &mut Entry,
    ) -> Result<(), RunnerError> {
        if entry.phase != Phase::Settled || entry.checkpoint().is_none() {
            return Err(RunnerError::new(
                "Only a settled checkpoint report may be acknowledged",
            ));
        }
        entry.checkpoint_settlement_ack = true;
        self.save(entry)
    }

    pub(crate) fn intent(
        &mut self,
        assignment: Assignment,
        mapping: EnvironmentMapping,
    ) -> Result<Entry, RunnerError> {
        if let Some(previous) = self.get(&assignment.attempt_id)? {
            if previous.assignment.generation != assignment.generation
                || previous.assignment.runner_id != assignment.runner_id
                || previous.assignment.job.id != assignment.job.id
                || previous.assignment.job.project_id != assignment.job.project_id
                || previous.assignment.job.environment_id != assignment.job.environment_id
                || previous.assignment.job.input != assignment.job.input
                || previous.assignment.job.checkpoint != assignment.job.checkpoint
                || previous.mapping != mapping
            {
                return Err(RunnerError::new(
                    "Hub attempt or its local authority changed",
                ));
            }
            return Ok(previous);
        }
        let count: u64 = self
            .db
            .query_row("SELECT COUNT(*) FROM runner_attempts", [], |row| row.get(0))
            .map_err(error)?;
        if count >= 10000 {
            return Err(RunnerError::new(
                "Runner journal capacity reached; retain its evidence before configuring a new execution identity",
            ));
        }
        let entry = Entry {
            assignment,
            mapping,
            run_id: Ulid::new(),
            phase: Phase::Intent,
            report: None,
            approval_report: None,
            fallback_report: None,
            local_checkpoint: None,
            checkpoint_settlement_ack: false,
            external: None,
        };
        let encoded = serde_json::to_string(&entry).map_err(error)?;
        self.db
            .execute(
                "INSERT INTO runner_attempts VALUES(?1,?2,?3)",
                params![
                    entry.assignment.attempt_id,
                    entry.assignment.generation,
                    encoded
                ],
            )
            .map_err(error)?;
        Ok(entry)
    }

    fn save(&mut self, entry: &Entry) -> Result<(), RunnerError> {
        let encoded = serde_json::to_string(entry).map_err(error)?;
        if encoded.len() > 1024 * 1024 {
            return Err(RunnerError::new("Runner journal receipt exceeds its bound"));
        }
        if self
            .db
            .execute(
                "UPDATE runner_attempts SET entry_json=?3 WHERE attempt_id=?1 AND generation=?2",
                params![
                    entry.assignment.attempt_id,
                    entry.assignment.generation,
                    encoded
                ],
            )
            .map_err(error)?
            != 1
        {
            return Err(RunnerError::new("Runner journal attempt is missing"));
        }
        Ok(())
    }

    /// Commit this before calling anything that could start a provider or tool effect.
    pub(crate) fn executing(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        if entry.phase != Phase::Intent {
            return Err(RunnerError::new(
                "Only a durable unexecuted intent may start",
            ));
        }
        entry.phase = Phase::Executing;
        self.save(entry)
    }
    pub(crate) fn external_execution(
        &mut self,
        entry: &mut Entry,
        request_id: Ulid,
    ) -> Result<(), RunnerError> {
        if entry.phase != Phase::Intent {
            return Err(RunnerError::new("This local admission is already used"));
        }
        entry.external = Some(super::external::ExternalEvidence { request_id });
        entry.phase = Phase::Executing;
        self.save(entry)
    }

    pub(crate) fn outcome(&mut self, entry: &mut Entry, report: Report) -> Result<(), RunnerError> {
        self.outcome_with_checkpoint(entry, report, None)
    }

    pub(crate) fn outcome_with_checkpoint(
        &mut self,
        entry: &mut Entry,
        report: Report,
        local_checkpoint: Option<serde_json::Value>,
    ) -> Result<(), RunnerError> {
        if !matches!(entry.phase, Phase::Intent | Phase::Executing) {
            return Err(RunnerError::new(
                "This attempt already has durable execution evidence",
            ));
        }
        entry.phase = Phase::ReportPending;
        entry.local_checkpoint = local_checkpoint.or_else(|| match &report.outcome {
            ReportOutcome::YieldToChild { checkpoint, .. } => Some(checkpoint.clone()),
            _ => None,
        });
        entry.report = Some(report);
        self.save(entry)
    }

    pub(crate) fn uncertain(&mut self, entry: &mut Entry, reason: &str) -> Result<(), RunnerError> {
        if entry.phase != Phase::Executing {
            return Err(RunnerError::new(
                "Only possibly executed work may be classified as uncertain",
            ));
        }
        entry.phase = Phase::Uncertain;
        entry.report = Some(Report::for_assignment(
            &entry.assignment,
            "uncertain",
            ReportOutcome::Uncertain {
                reason: reason.into(),
            },
        ));
        self.save(entry)
    }

    pub(crate) fn settled(&mut self, entry: &mut Entry) -> Result<(), RunnerError> {
        if entry.phase != Phase::ReportPending {
            return Err(RunnerError::new(
                "No durable final report exists for this attempt",
            ));
        }
        entry.phase = Phase::Settled;
        self.save(entry)
    }

    pub(crate) fn reconciled(
        &mut self,
        entry: &mut Entry,
        report: Report,
    ) -> Result<(), RunnerError> {
        if entry.phase != Phase::Uncertain
            || !matches!(
                report.outcome,
                ReportOutcome::Finished {
                    success: false,
                    resources_released: true,
                    ..
                }
            )
        {
            return Err(RunnerError::new(
                "Only an explicitly reconciled unknown attempt may release resources",
            ));
        }
        // Retain the original uncertainty evidence, as for an unacknowledged handoff.
        entry.fallback_report = entry.report.take();
        entry.report = Some(report);
        entry.phase = Phase::ReportPending;
        self.save(entry)
    }

    pub(crate) fn retain_rejected_yield_fallback(
        &mut self,
        entry: &mut Entry,
        report: Report,
    ) -> Result<(), RunnerError> {
        if entry.phase != Phase::ReportPending
            || !entry
                .report
                .as_ref()
                .is_some_and(|report| matches!(report.outcome, ReportOutcome::YieldToChild { .. }))
        {
            return Err(RunnerError::new(
                "Only a rejected durable child handoff may be replaced",
            ));
        }
        if entry
            .fallback_report
            .as_ref()
            .is_some_and(|previous| *previous != report)
        {
            return Err(RunnerError::new(
                "The pending child-handoff fallback changed",
            ));
        }
        entry.fallback_report = Some(report);
        self.save(entry)
    }

    pub(crate) fn approval(
        &mut self,
        entry: &mut Entry,
        report: Report,
    ) -> Result<(), RunnerError> {
        if entry.phase != Phase::Executing
            || !matches!(report.outcome, ReportOutcome::ApprovalRequested { .. })
        {
            return Err(RunnerError::new("Approval belongs to an executing attempt"));
        }
        entry.approval_report = Some(report);
        self.save(entry)
    }
}

fn error(error: impl std::fmt::Display) -> RunnerError {
    RunnerError::new(format!("Runner journal unavailable: {error}"))
}
