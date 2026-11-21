//! Versioned local metadata storage. Scan reports are append-only snapshots;
//! importing and rescanning a project never changes its source directory.
use crate::{JobRecord, Project, RunRecord, RunStatus, ScanReport, stable_id, timestamp};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    time::Duration,
};

const SCHEMA_VERSION: i64 = 4;
const MAX_HISTORY_ITEMS: usize = 1_000;

pub struct Store {
    connection: Connection,
}

impl Store {
    pub(crate) fn intelligence_find<T: DeserializeOwned>(
        &self,
        table: IntelligenceTable,
        id: &str,
    ) -> Result<Option<T>> {
        let sql = format!("SELECT record_json FROM {} WHERE id=?1", table.name());
        let value: Option<String> = self
            .connection
            .query_row(&sql, [id], |row| row.get(0))
            .optional()?;
        value
            .map(|value| decode_json(&value, "project intelligence"))
            .transpose()
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Unable to create metadata directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("Unable to open metadata database {}", path.display()))?;
        Self::initialize(connection)
    }

    pub fn open_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "Metadata schema version {version} is newer than supported version {SCHEMA_VERSION}; use a newer Verifact build"
        );
        if version < SCHEMA_VERSION {
            let transaction = connection.unchecked_transaction()?;
            if version == 0 {
                transaction.execute_batch(
                "CREATE TABLE projects (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    path TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE project_scans (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    project_id TEXT NOT NULL REFERENCES projects(id),
                    scanned_at TEXT NOT NULL,
                    report_json TEXT NOT NULL
                );
                CREATE INDEX project_scans_latest ON project_scans(project_id, sequence DESC);
                CREATE TABLE background_jobs (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_id TEXT NOT NULL REFERENCES projects(id),
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    finished_at TEXT,
                    message TEXT NOT NULL
                );
                CREATE INDEX background_jobs_project ON background_jobs(project_id, started_at DESC, id);
                CREATE TABLE settings (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                );",
                )?;
            }
            if version < 2 {
                transaction
                    .execute_batch("ALTER TABLE background_jobs ADD COLUMN owner_pid INTEGER;")?;
            }
            if version < 3 {
                transaction.execute_batch(
                    "CREATE TABLE test_runs (
                        id TEXT PRIMARY KEY NOT NULL,
                        project_id TEXT NOT NULL REFERENCES projects(id),
                        status TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        finished_at TEXT,
                        owner_pid INTEGER,
                        cancel_requested INTEGER NOT NULL DEFAULT 0,
                        record_json TEXT NOT NULL
                    );
                    CREATE INDEX test_runs_project ON test_runs(project_id, created_at DESC, id);
                    CREATE INDEX test_runs_active ON test_runs(status, created_at);",
                )?;
            }
            if version < 4 {
                transaction.execute_batch(
                    "CREATE TABLE knowledge_facts(id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id), updated_at TEXT NOT NULL, record_json TEXT NOT NULL, UNIQUE(project_id,id));
                     CREATE INDEX knowledge_facts_project ON knowledge_facts(project_id,updated_at DESC,id);
                     CREATE TABLE knowledge_relationships(id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id), from_id TEXT NOT NULL, to_id TEXT NOT NULL, updated_at TEXT NOT NULL, record_json TEXT NOT NULL,
                        FOREIGN KEY(project_id,from_id) REFERENCES knowledge_facts(project_id,id), FOREIGN KEY(project_id,to_id) REFERENCES knowledge_facts(project_id,id));
                     CREATE INDEX knowledge_relationships_project ON knowledge_relationships(project_id,updated_at DESC,id);
                     CREATE TABLE findings(id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id), updated_at TEXT NOT NULL, record_json TEXT NOT NULL, UNIQUE(project_id,id));
                     CREATE INDEX findings_project ON findings(project_id,updated_at DESC,id);
                     CREATE TABLE investigations(id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES projects(id), finding_id TEXT, updated_at TEXT NOT NULL, record_json TEXT NOT NULL,
                        FOREIGN KEY(project_id,finding_id) REFERENCES findings(project_id,id));
                     CREATE INDEX investigations_project ON investigations(project_id,updated_at DESC,id);",
                )?;
            }
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { connection })
    }

    pub fn add_project(&self, path: impl AsRef<Path>) -> Result<Project> {
        let path = std::fs::canonicalize(path.as_ref()).with_context(|| {
            format!(
                "Unable to locate project directory {}",
                path.as_ref().display()
            )
        })?;
        ensure!(
            path.is_dir(),
            "Project path is not a directory: {}",
            path.display()
        );
        let path_text = path
            .to_str()
            .context("Project path must contain valid UTF-8")?;
        let id = stable_id(&["project", path_text]);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(path_text)
            .to_owned();
        self.connection.execute(
            "INSERT INTO projects(id, name, path, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO NOTHING",
            params![id, name, path_text, timestamp()],
        )?;
        self.connection
            .query_row(
                "SELECT id, name, path, created_at FROM projects WHERE path = ?1",
                [path_text],
                project_from_row,
            )
            .context("Unable to retrieve the registered project")
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, path, created_at FROM projects ORDER BY name COLLATE NOCASE, path",
        )?;
        statement
            .query_map([], project_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Unable to read registered projects")
    }

    pub fn project(&self, id: &str) -> Result<Option<Project>> {
        self.connection
            .query_row(
                "SELECT id, name, path, created_at FROM projects WHERE id = ?1",
                [id],
                project_from_row,
            )
            .optional()
            .context("Unable to read project")
    }

    /// Update only supplied values; registration and scan snapshots keep their identity.
    pub fn configure_project(
        &self,
        id: &str,
        name: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Project> {
        let name = name.map(str::trim);
        ensure!(
            name.is_none_or(|name| !name.is_empty()),
            "Project name cannot be empty"
        );
        let base_url = base_url.map(validate_base_url).transpose()?;
        let transaction = self.connection.unchecked_transaction()?;
        let mut project = transaction
            .query_row(
                "SELECT id, name, path, created_at FROM projects WHERE id = ?1",
                [id],
                project_from_row,
            )
            .optional()?
            .with_context(|| format!("Unknown project: {id}"))?;
        if let Some(name) = name {
            transaction.execute(
                "UPDATE projects SET name = ?1 WHERE id = ?2",
                params![name, id],
            )?;
            project.name = name.to_owned();
        }
        if let Some(base_url) = base_url {
            transaction.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![project_url_key(id), base_url],
            )?;
        }
        transaction
            .commit()
            .context("Unable to save project configuration")?;
        Ok(project)
    }

    pub fn project_base_url(&self, id: &str) -> Result<Option<String>> {
        ensure!(self.project(id)?.is_some(), "Unknown project: {id}");
        self.get_setting(&project_url_key(id))?
            .map(|value| validate_base_url(&value))
            .transpose()
    }

    pub fn save_scan(&self, report: &ScanReport) -> Result<()> {
        validate_scan(report)?;
        let report_json =
            serde_json::to_string(report).context("Unable to serialize scan report")?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO project_scans(project_id, scanned_at, report_json) VALUES (?1, ?2, ?3)",
            params![report.project_id, report.scanned_at, report_json],
        ).context("Unable to persist scan report; register its project first")?;
        transaction
            .commit()
            .context("Unable to finalize scan report")
    }

    pub fn latest_scan(&self, project_id: &str) -> Result<Option<ScanReport>> {
        let json: Option<String> = self.connection.query_row(
            "SELECT report_json FROM project_scans WHERE project_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [project_id], |row| row.get(0),
        ).optional()?;
        json.map(|json| decode_json(&json, "scan report"))
            .transpose()
    }

    /// Returns newest snapshots first. Results are capped even for oversized requests.
    pub fn scan_history(&self, project_id: &str, limit: usize) -> Result<Vec<ScanReport>> {
        let mut statement = self.connection.prepare(
            "SELECT report_json FROM project_scans WHERE project_id = ?1 ORDER BY sequence DESC LIMIT ?2",
        )?;
        let reports = statement
            .query_map(params![project_id, history_limit(limit)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        reports
            .iter()
            .map(|json| decode_json(json, "scan report"))
            .collect()
    }

    pub fn save_job(&self, job: &JobRecord) -> Result<()> {
        ensure!(
            !job.id.is_empty()
                && !job.project_id.is_empty()
                && !job.kind.is_empty()
                && !job.status.is_empty()
                && !job.started_at.is_empty(),
            "Job identity, kind, status and start time are required"
        );
        ensure!(
            job.owner_pid
                .is_none_or(|pid| pid > 0 && i32::try_from(pid).is_ok()),
            "Job owner PID must be positive and fit the operating system PID range"
        );
        let changed = self.connection.execute(
            "INSERT INTO background_jobs(id, project_id, kind, status, started_at, finished_at, message, owner_pid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status,
                 finished_at = excluded.finished_at, message = excluded.message
             WHERE background_jobs.project_id = excluded.project_id AND background_jobs.kind = excluded.kind
                 AND background_jobs.owner_pid IS excluded.owner_pid",
            params![job.id, job.project_id, job.kind, job.status, job.started_at, job.finished_at, job.message, job.owner_pid],
        ).context("Unable to persist background job; register its project first")?;
        ensure!(
            changed == 1,
            "Existing job identity belongs to another project, job kind or process owner"
        );
        Ok(())
    }

    pub fn jobs(&self, project_id: &str, limit: usize) -> Result<Vec<JobRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_id, kind, status, started_at, finished_at, message, owner_pid
             FROM background_jobs WHERE project_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        statement
            .query_map(params![project_id, history_limit(limit)], |row| {
                Ok(JobRecord {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    kind: row.get(2)?,
                    status: row.get(3)?,
                    started_at: row.get(4)?,
                    finished_at: row.get(5)?,
                    message: row.get(6)?,
                    owner_pid: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Unable to read background jobs")
    }

    /// Recover only jobs whose known Unix process owner demonstrably exited.
    /// Legacy/unknown owners and non-Unix platforms remain untouched. A live
    /// PID is preserved even if it might have been recycled since job creation.
    pub fn recover_interrupted_jobs(&self) -> Result<usize> {
        let owners = {
            let mut statement = self.connection.prepare(
                "SELECT id, owner_pid FROM background_jobs
                 WHERE status IN ('queued', 'preparing', 'running')",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let transaction = self.connection.unchecked_transaction()?;
        let mut recovered = 0;
        let finished_at = timestamp();
        for (id, owner_pid) in owners {
            if owner_is_demonstrably_dead(owner_pid) {
                recovered += transaction.execute(
                    "UPDATE background_jobs SET status = 'interrupted', finished_at = ?1,
                     message = 'The owning Verifact process exited before this operation completed; retry the operation.'
                     WHERE id = ?2 AND owner_pid = ?3 AND status IN ('queued', 'preparing', 'running')",
                    params![finished_at, id, owner_pid],
                )?;
            }
        }
        transaction
            .commit()
            .context("Unable to recover interrupted background jobs")?;
        Ok(recovered)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .context("Unable to read application setting")
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        ensure!(!key.is_empty(), "Setting key cannot be empty");
        self.connection.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub(crate) fn insert_run(&self, record: &RunRecord) -> Result<()> {
        ensure!(
            record.status == RunStatus::Queued && record.finished_at.is_none(),
            "New runs must be queued"
        );
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM test_runs WHERE finished_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            count < 128,
            "Execution queue is full; wait for or cancel existing work"
        );
        {
            let mut statement = transaction.prepare(
                "SELECT record_json FROM test_runs WHERE project_id=?1 AND finished_at IS NULL",
            )?;
            let active = statement
                .query_map([&record.project_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for json in active {
                let existing: RunRecord = decode_json(&json, "test run")?;
                ensure!(
                    !existing
                        .plan
                        .candidate_ids
                        .iter()
                        .any(|id| record.plan.candidate_ids.contains(id)),
                    "A selected candidate already has queued or running execution; wait or cancel it first"
                );
            }
        }
        transaction.execute(
            "INSERT INTO test_runs(id,project_id,status,created_at,owner_pid,record_json) VALUES (?1,?2,?3,?4,?5,?6)",
            params![record.id, record.project_id, record.status.as_str(), record.queued_at, record.owner_pid, serde_json::to_string(record)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn run(&self, id: &str) -> Result<Option<RunRecord>> {
        let value: Option<(String, bool)> = self
            .connection
            .query_row(
                "SELECT record_json,cancel_requested FROM test_runs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        value
            .map(|(json, cancel)| {
                let mut record: RunRecord = decode_json(&json, "test run")?;
                record.cancel_requested = cancel;
                Ok(record)
            })
            .transpose()
    }

    pub fn run_history(&self, project_id: &str, limit: usize) -> Result<Vec<RunRecord>> {
        let mut statement = self.connection.prepare("SELECT id FROM test_runs WHERE project_id=?1 ORDER BY created_at DESC,id DESC LIMIT ?2")?;
        let ids = statement
            .query_map(params![project_id, history_limit(limit)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| self.run(id)?.context("Run disappeared during history read"))
            .collect()
    }

    pub fn active_runs(&self) -> Result<Vec<RunRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM test_runs WHERE finished_at IS NULL ORDER BY created_at,id LIMIT 128",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| self.run(id)?.context("Active run disappeared"))
            .collect()
    }

    pub(crate) fn update_run(&self, record: &RunRecord) -> Result<()> {
        ensure!(
            record.status.is_final() == record.finished_at.is_some(),
            "Run terminal state and completion time must agree"
        );
        let changed = self.connection.execute(
            "UPDATE test_runs SET status=?1,finished_at=?2,record_json=?3 WHERE id=?4 AND project_id=?5 AND finished_at IS NULL",
            params![record.status.as_str(),record.finished_at,serde_json::to_string(record)?,record.id,record.project_id],
        )?;
        ensure!(
            changed == 1,
            "Run is missing or already finalized; finalized evidence is immutable"
        );
        Ok(())
    }

    pub(crate) fn claim_run(&self, id: &str, max_concurrency: usize) -> Result<bool> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let running: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM test_runs WHERE status='running'",
            [],
            |row| row.get(0),
        )?;
        let mut effective_limit = max_concurrency;
        {
            let mut statement = transaction.prepare("SELECT record_json FROM test_runs WHERE finished_at IS NULL AND cancel_requested=0")?;
            let active = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for json in active {
                let record: RunRecord = decode_json(&json, "test run")?;
                effective_limit = effective_limit.min(record.plan.options.max_concurrency);
            }
        }
        if running >= effective_limit as i64 {
            return Ok(false);
        }
        let changed = transaction.execute("UPDATE test_runs SET status='running' WHERE id=?1 AND status='queued' AND cancel_requested=0", [id])?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub(crate) fn request_run_cancel(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE test_runs SET cancel_requested=1 WHERE id=?1 AND finished_at IS NULL",
            [id],
        )?;
        ensure!(self.run(id)?.is_some(), "Unknown run: {id}");
        Ok(())
    }

    pub fn recover_interrupted_runs(&self) -> Result<usize> {
        let mut statement = self
            .connection
            .prepare("SELECT record_json FROM test_runs WHERE finished_at IS NULL")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut recovered = 0;
        for json in rows {
            let mut record: RunRecord = decode_json(&json, "test run")?;
            if owner_is_demonstrably_dead(record.owner_pid.map(i64::from)) {
                record.status = RunStatus::InfrastructureError;
                record.finished_at = Some(timestamp());
                record.message = Some("The owning Verifact process exited. Descendant process state and partial artifacts are unverified.".into());
                // A competing recovery may have finalized the same row already.
                match self.update_run(&record) {
                    Ok(()) => recovered += 1,
                    Err(error)
                        if self
                            .run(&record.id)?
                            .is_some_and(|run| run.status.is_final()) =>
                    {
                        let _ = error;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(recovered)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum IntelligenceTable {
    Facts,
    Relationships,
    Findings,
    Investigations,
}
impl IntelligenceTable {
    fn name(self) -> &'static str {
        match self {
            Self::Facts => "knowledge_facts",
            Self::Relationships => "knowledge_relationships",
            Self::Findings => "findings",
            Self::Investigations => "investigations",
        }
    }
}
impl Store {
    pub(crate) fn intelligence_list<T: DeserializeOwned>(
        &self,
        table: IntelligenceTable,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<T>> {
        let sql = format!(
            "SELECT record_json FROM {} WHERE project_id=?1 ORDER BY updated_at DESC,id LIMIT ?2",
            table.name()
        );
        let mut statement = self.connection.prepare(&sql)?;
        let values = statement
            .query_map(params![project_id, history_limit(limit)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
            .iter()
            .map(|value| decode_json(value, "project intelligence"))
            .collect()
    }
    pub(crate) fn intelligence_get<T: DeserializeOwned>(
        &self,
        table: IntelligenceTable,
        project_id: &str,
        id: &str,
    ) -> Result<Option<T>> {
        let sql = format!(
            "SELECT record_json FROM {} WHERE project_id=?1 AND id=?2",
            table.name()
        );
        let value: Option<String> = self
            .connection
            .query_row(&sql, params![project_id, id], |row| row.get(0))
            .optional()?;
        value
            .map(|value| decode_json(&value, "project intelligence"))
            .transpose()
    }
    pub(crate) fn intelligence_save<T: serde::Serialize + DeserializeOwned>(
        &self,
        table: IntelligenceTable,
        value: T,
    ) -> Result<T> {
        let mut value = serde_json::to_value(value)?;
        let id = value["id"]
            .as_str()
            .context("Intelligence identity required")?
            .to_owned();
        let project_id = value["project_id"]
            .as_str()
            .context("Intelligence project required")?
            .to_owned();
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let sql = format!(
            "SELECT project_id,record_json FROM {} WHERE id=?1",
            table.name()
        );
        let previous: Option<(String, String)> = transaction
            .query_row(&sql, [&id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let old = if let Some((owner, json)) = previous {
            ensure!(
                owner == project_id,
                "Intelligence identity belongs to another project"
            );
            serde_json::from_str::<serde_json::Value>(&json)?
        } else {
            serde_json::Value::Null
        };
        let now = timestamp();
        value["created_at"] = old
            .get("created_at")
            .cloned()
            .unwrap_or_else(|| now.clone().into());
        value["updated_at"] = now.clone().into();
        value["version"] = old["version"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .context("Intelligence version exhausted")?
            .into();
        let json = serde_json::to_string(&value)?;
        ensure!(
            json.len() <= 65_536,
            "Intelligence record exceeds the 64 KiB limit"
        );
        match table {
            IntelligenceTable::Relationships => {
                let from = value["from_id"]
                    .as_str()
                    .context("Relationship source required")?;
                let to = value["to_id"]
                    .as_str()
                    .context("Relationship target required")?;
                transaction.execute("INSERT INTO knowledge_relationships(id,project_id,from_id,to_id,updated_at,record_json) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET from_id=excluded.from_id,to_id=excluded.to_id,updated_at=excluded.updated_at,record_json=excluded.record_json",params![id,project_id,from,to,now,json])?;
            }
            IntelligenceTable::Investigations => {
                let finding = value["finding_id"].as_str();
                transaction.execute("INSERT INTO investigations(id,project_id,finding_id,updated_at,record_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET finding_id=excluded.finding_id,updated_at=excluded.updated_at,record_json=excluded.record_json",params![id,project_id,finding,now,json])?;
            }
            _ => {
                let sql = format!(
                    "INSERT INTO {}(id,project_id,updated_at,record_json) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,record_json=excluded.record_json",
                    table.name()
                );
                transaction.execute(&sql, params![id, project_id, now, json])?;
            }
        }
        transaction.commit()?;
        Ok(serde_json::from_value(value)?)
    }
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
    pub(crate) fn recent_runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM test_runs ORDER BY created_at DESC,id LIMIT ?1")?;
        let ids = statement
            .query_map([history_limit(limit)], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| self.run(id)?.context("Run disappeared"))
            .collect()
    }
    pub(crate) fn active_jobs(&self) -> Result<Vec<JobRecord>> {
        let mut statement=self.connection.prepare("SELECT id,project_id,kind,status,started_at,finished_at,message,owner_pid FROM background_jobs WHERE status IN ('queued','preparing','running') ORDER BY started_at DESC LIMIT 128")?;
        let jobs = statement
            .query_map([], |row| {
                Ok(JobRecord {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    kind: row.get(2)?,
                    status: row.get(3)?,
                    started_at: row.get(4)?,
                    finished_at: row.get(5)?,
                    message: row.get(6)?,
                    owner_pid: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(jobs)
    }
}

fn project_url_key(id: &str) -> String {
    format!("project:{id}:base_url")
}

fn validate_base_url(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(
        !value.chars().any(char::is_control),
        "Project URL cannot contain control characters"
    );
    let (scheme, remainder) = value
        .split_once("://")
        .context("Project URL must be an absolute HTTP or HTTPS URL")?;
    ensure!(
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"),
        "Project URL must use HTTP or HTTPS"
    );
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    ensure!(!authority.is_empty(), "Project URL requires a host");
    ensure!(
        !authority.contains('@'),
        "Project URL must not contain credentials"
    );
    let url = url::Url::parse(value).context("Invalid project URL")?;
    ensure!(url.host_str().is_some(), "Project URL requires a host");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Project URL must not contain credentials"
    );
    Ok(url.into())
}

#[cfg(unix)]
fn owner_is_demonstrably_dead(owner_pid: Option<i64>) -> bool {
    let Some(pid) = owner_pid
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    // SAFETY: a positive validated PID with signal zero only probes existence;
    // it sends no signal. Permission denial and every unknown error preserve
    // the job; only ESRCH establishes that the owner does not currently exist.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn owner_is_demonstrably_dead(_owner_pid: Option<i64>) -> bool {
    false
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        created_at: row.get(3)?,
    })
}

fn history_limit(limit: usize) -> i64 {
    limit.min(MAX_HISTORY_ITEMS) as i64
}

fn decode_json<T: DeserializeOwned>(json: &str, label: &str) -> Result<T> {
    serde_json::from_str(json).with_context(|| format!("Stored {label} is malformed"))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_))),
        "Scan evidence path must stay within its project: {}",
        path.display()
    );
    Ok(())
}

fn validate_scan(report: &ScanReport) -> Result<()> {
    ensure!(
        !report.project_id.is_empty() && !report.scanned_at.is_empty(),
        "Scan project identity and timestamp are required"
    );
    let mut frameworks = HashSet::new();
    for framework in &report.frameworks {
        ensure!(
            !framework.id.is_empty() && frameworks.insert(framework.id.as_str()),
            "Framework identities must be nonempty and unique"
        );
        validate_relative_path(&framework.package_path)?;
        for path in &framework.configurations {
            validate_relative_path(path)?;
        }
    }
    let mut candidates = HashSet::new();
    for candidate in &report.candidates {
        ensure!(
            !candidate.id.is_empty() && candidates.insert(candidate.id.as_str()),
            "Test candidate identities must be nonempty and unique"
        );
        ensure!(
            frameworks.contains(candidate.framework_id.as_str()),
            "Test candidate refers to a framework absent from this scan"
        );
        ensure!(
            !candidate.discovery_kind.is_empty(),
            "Test candidate discovery kind is required"
        );
        ensure!(
            !candidate.file.as_os_str().is_empty(),
            "Test candidate file is required"
        );
        validate_relative_path(&candidate.file)?;
    }
    for warning in &report.warnings {
        validate_relative_path(&warning.path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Framework, ScanWarning, TestCandidate};

    fn queued_run(project: &Project, id: &str, owner_pid: Option<u32>) -> RunRecord {
        RunRecord {
            id: id.into(),
            project_id: project.id.clone(),
            plan: crate::ExecutionPlan {
                id: id.into(),
                project_id: project.id.clone(),
                project_path: project.path.clone(),
                candidate_ids: vec![id.into()],
                commands: vec![],
                options: crate::RunOptions::default(),
                planned_at: timestamp(),
                revision: None,
                working_tree: None,
                source_fingerprint: None,
            },
            status: RunStatus::Queued,
            queued_at: timestamp(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            exit_code: None,
            results: vec![],
            artifacts: vec![],
            message: None,
            owner_pid,
            cancel_requested: false,
            output_truncated: false,
            revision: None,
            working_tree: None,
            source_state: None,
        }
    }

    #[test]
    fn schema_two_migration_preserves_registry_settings_and_protected_job_owner() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let project;
        {
            let store = Store::open(&path)?;
            project = store.add_project(directory.path())?;
            store.set_setting("appearance", "dark")?;
            let mut entry = job(&project, "live", "running");
            entry.owner_pid = Some(std::process::id());
            store.save_job(&entry)?;
            store
                .connection
                .execute_batch("DROP TABLE knowledge_relationships; DROP TABLE investigations; DROP TABLE findings; DROP TABLE knowledge_facts; DROP TABLE test_runs; PRAGMA user_version=2;")?;
        }
        let store = Store::open(&path)?;
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        assert_eq!(store.project(&project.id)?, Some(project.clone()));
        assert_eq!(store.get_setting("appearance")?.as_deref(), Some("dark"));
        assert_eq!(
            store.jobs(&project.id, 1)?[0].owner_pid,
            Some(std::process::id())
        );
        store.insert_run(&queued_run(&project, "first", Some(std::process::id())))?;
        assert_eq!(store.run_history(&project.id, 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn schema_three_migration_preserves_legacy_execution_without_source_fingerprint() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let record;
        {
            let store = Store::open(&path)?;
            let project = store.add_project(directory.path())?;
            record = queued_run(&project, "legacy-run", Some(std::process::id()));
            store.insert_run(&record)?;
            let mut value = serde_json::to_value(&record)?;
            value
                .as_object_mut()
                .context("Record object")?
                .remove("source_state");
            value["plan"]
                .as_object_mut()
                .context("Plan object")?
                .remove("source_fingerprint");
            store.connection.execute(
                "UPDATE test_runs SET record_json=?1 WHERE id=?2",
                params![serde_json::to_string(&value)?, record.id],
            )?;
            store.connection.execute_batch("DROP TABLE knowledge_relationships; DROP TABLE investigations; DROP TABLE findings; DROP TABLE knowledge_facts; PRAGMA user_version=3;")?;
        }
        let store = Store::open(&path)?;
        assert_eq!(store.schema_version()?, 4);
        assert_eq!(store.run(&record.id)?, Some(record));
        Ok(())
    }

    #[test]
    fn finalized_runs_are_immutable_and_recovery_preserves_live_or_unknown_owners() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        let mut record = queued_run(&project, "finished", Some(std::process::id()));
        store.insert_run(&record)?;
        record.status = RunStatus::Blocked;
        record.finished_at = Some(timestamp());
        store.update_run(&record)?;
        assert!(store.update_run(&record).is_err());
        store.request_run_cancel(&record.id)?;
        assert_eq!(store.run(&record.id)?, Some(record));
        store.insert_run(&queued_run(&project, "live", Some(std::process::id())))?;
        store.insert_run(&queued_run(&project, "unknown", None))?;
        assert_eq!(store.recover_interrupted_runs()?, 0);
        assert_eq!(store.run("live")?.context("Run")?.status, RunStatus::Queued);
        assert_eq!(
            store.run("unknown")?.context("Run")?.status,
            RunStatus::Queued
        );
        Ok(())
    }

    #[test]
    fn admission_honors_the_lowest_active_concurrency_limit_across_connections() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let store = Store::open(&path)?;
        let other = Store::open(&path)?;
        let project = store.add_project(directory.path())?;
        let mut first = queued_run(&project, "first", Some(std::process::id()));
        first.plan.options.max_concurrency = 1;
        let mut second = queued_run(&project, "second", Some(std::process::id()));
        second.plan.options.max_concurrency = 4;
        store.insert_run(&first)?;
        other.insert_run(&second)?;
        assert!(store.claim_run(&first.id, 1)?);
        assert!(!other.claim_run(&second.id, 4)?);
        first.status = RunStatus::Cancelled;
        first.finished_at = Some(timestamp());
        store.update_run(&first)?;
        assert!(other.claim_run(&second.id, 4)?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execution_recovery_finalizes_a_demonstrably_dead_owner() -> Result<()> {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()?;
        let owner = child.id();
        child.wait()?;
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        store.insert_run(&queued_run(&project, "orphan", Some(owner)))?;
        assert_eq!(store.recover_interrupted_runs()?, 1);
        let record = store.run("orphan")?.context("Recovered run")?;
        assert_eq!(record.status, RunStatus::InfrastructureError);
        assert!(record.finished_at.is_some());
        assert_eq!(store.recover_interrupted_runs()?, 0);
        Ok(())
    }

    #[test]
    fn project_configuration_rolls_back_name_when_url_write_fails() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        store.connection.execute_batch(
            "CREATE TRIGGER reject_url BEFORE INSERT ON settings
             BEGIN SELECT RAISE(ABORT, 'simulated storage failure'); END;",
        )?;
        assert!(
            store
                .configure_project(&project.id, Some("New name"), Some("https://example.test/"))
                .is_err()
        );
        assert_eq!(store.project(&project.id)?, Some(project.clone()));
        assert_eq!(store.project_base_url(&project.id)?, None);
        Ok(())
    }

    fn report(project: &Project, scanned_at: &str) -> ScanReport {
        ScanReport {
            project_id: project.id.clone(),
            scanned_at: scanned_at.into(),
            frameworks: vec![Framework {
                id: "framework-one".into(),
                package_path: PathBuf::from("."),
                name: "Vitest".into(),
                language: "JavaScript".into(),
                evidence: vec!["package.json contains vitest".into()],
                configurations: vec![PathBuf::from("vitest.config.ts")],
                suggested_command: Some(vec!["npm".into(), "test".into()]),
            }],
            candidates: vec![TestCandidate {
                id: "candidate-one".into(),
                framework_id: "framework-one".into(),
                file: PathBuf::from("tests/cart.test.ts"),
                display_name: "cart.test.ts".into(),
                discovery_kind: "file_candidate".into(),
            }],
            warnings: vec![ScanWarning {
                path: PathBuf::from("package.json"),
                message: "Example evidence warning".into(),
            }],
            files_inspected: 3,
            revision: Some("abc123".into()),
            working_tree: Some("clean".into()),
        }
    }

    fn job(project: &Project, id: &str, status: &str) -> JobRecord {
        JobRecord {
            id: id.into(),
            project_id: project.id.clone(),
            kind: "scan".into(),
            status: status.into(),
            started_at: "2026-11-21T16:00:00Z".into(),
            finished_at: None,
            message: "Scanning project".into(),
            owner_pid: None,
        }
    }

    #[test]
    fn projects_settings_and_snapshots_survive_reopen() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("state/verifact.sqlite");
        let source = directory.path().join("source");
        std::fs::create_dir(&source)?;
        let (project, first, second) = {
            let store = Store::open(&database)?;
            let project = store.add_project(&source)?;
            let first = report(&project, "same-timestamp");
            let mut second = report(&project, "same-timestamp");
            second.candidates.clear();
            store.save_scan(&first)?;
            store.save_scan(&second)?;
            store.set_setting("selected_project", &project.id)?;
            (project, first, second)
        };
        let store = Store::open(&database)?;
        assert_eq!(store.projects()?, vec![project.clone()]);
        assert_eq!(store.project(&project.id)?, Some(project.clone()));
        assert_eq!(
            store.get_setting("selected_project")?,
            Some(project.id.clone())
        );
        assert_eq!(store.latest_scan(&project.id)?, Some(second.clone()));
        assert_eq!(
            store.scan_history(&project.id, 10)?,
            vec![second.clone(), first]
        );
        assert_eq!(store.scan_history(&project.id, 1)?, vec![second]);
        assert!(store.scan_history(&project.id, 0)?.is_empty());
        assert!(store.project("missing")?.is_none());
        assert!(store.latest_scan("missing")?.is_none());
        Ok(())
    }

    #[test]
    fn canonical_directory_registration_is_idempotent_and_rejects_files() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        assert_eq!(store.add_project(directory.path().join("."))?, project);
        assert_eq!(store.projects()?.len(), 1);
        let file = directory.path().join("test.txt");
        std::fs::write(&file, "unchanged")?;
        assert!(store.add_project(&file).is_err());
        assert!(store.add_project(directory.path().join("missing")).is_err());
        assert_eq!(std::fs::read_to_string(file)?, "unchanged");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_original_directory_share_project_identity() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("source");
        let link = directory.path().join("linked");
        std::fs::create_dir(&source)?;
        std::os::unix::fs::symlink(&source, &link)?;
        let store = Store::open_memory()?;
        assert_eq!(store.add_project(&source)?, store.add_project(&link)?);
        Ok(())
    }

    #[test]
    fn invalid_or_orphaned_reports_do_not_replace_valid_snapshots() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        let valid = report(&project, "first");
        store.save_scan(&valid)?;
        let mut invalid = valid.clone();
        invalid.candidates[0].framework_id = "unknown".into();
        assert!(store.save_scan(&invalid).is_err());
        invalid = valid.clone();
        invalid.candidates.push(invalid.candidates[0].clone());
        assert!(store.save_scan(&invalid).is_err());
        invalid = valid.clone();
        invalid.candidates[0].file = PathBuf::from("../outside.test.ts");
        assert!(store.save_scan(&invalid).is_err());
        invalid = valid.clone();
        invalid.project_id = "unregistered".into();
        assert!(store.save_scan(&invalid).is_err());
        assert_eq!(store.latest_scan(&project.id)?, Some(valid));
        assert_eq!(store.scan_history(&project.id, 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn recovery_preserves_live_and_unknown_process_owners() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("metadata.sqlite");
        let store = Store::open(&database)?;
        let project = store.add_project(directory.path())?;
        let legacy = job(&project, "unknown-owner", "running");
        store.save_job(&legacy)?;
        let mut live = job(&project, "live-owner", "running");
        live.owner_pid = Some(std::process::id());
        store.save_job(&live)?;
        let mut completed = job(&project, "done", "completed");
        completed.finished_at = Some("2026-11-21T16:00:01Z".into());
        store.save_job(&completed)?;
        let second_connection = Store::open(&database)?;
        assert_eq!(second_connection.recover_interrupted_jobs()?, 0);
        let jobs = store.jobs(&project.id, 10)?;
        assert!(jobs.contains(&legacy));
        assert!(jobs.contains(&live));
        assert!(jobs.contains(&completed));
        assert!(store.jobs(&project.id, 0)?.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovery_marks_only_a_demonstrably_exited_owner_interrupted() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        // Run only the test harness's listing mode; no project-defined code.
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--list")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let owner_pid = child.id();
        ensure!(
            child.wait()?.success(),
            "Test harness listing process failed"
        );
        let mut interrupted = job(&project, "exited-owner", "running");
        interrupted.owner_pid = Some(owner_pid);
        store.save_job(&interrupted)?;
        assert_eq!(store.recover_interrupted_jobs()?, 1);
        let jobs = store.jobs(&project.id, 10)?;
        let recovered = jobs.first().context("Missing recovered job")?;
        assert_eq!(recovered.status, "interrupted");
        assert_eq!(recovered.owner_pid, Some(owner_pid));
        assert!(recovered.finished_at.is_some());
        assert_eq!(store.recover_interrupted_jobs()?, 0);
        Ok(())
    }

    #[test]
    fn invalid_owner_pids_are_rejected_without_process_group_probes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        for owner_pid in [0, u32::MAX] {
            let mut invalid = job(&project, "invalid-owner", "running");
            invalid.owner_pid = Some(owner_pid);
            assert!(store.save_job(&invalid).is_err());
        }
        for unknown in [None, Some(-1), Some(0), Some(i64::MAX)] {
            assert!(!owner_is_demonstrably_dead(unknown));
        }
        assert!(store.jobs(&project.id, 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn schema_one_migration_preserves_legacy_data_and_unknown_owners() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("schema-one.sqlite");
        let project = Project {
            id: "legacy-project".into(),
            name: "Legacy".into(),
            path: std::fs::canonicalize(directory.path())?,
            created_at: "earlier".into(),
        };
        let scan = report(&project, "earlier");
        {
            let connection = Connection::open(&database)?;
            // Frozen schema-1 fixture: owner_pid did not exist in this version.
            connection.execute_batch(
                "CREATE TABLE projects (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL,
                     path TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL);
                 CREATE TABLE project_scans (sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_id TEXT NOT NULL REFERENCES projects(id), scanned_at TEXT NOT NULL,
                     report_json TEXT NOT NULL);
                 CREATE INDEX project_scans_latest ON project_scans(project_id, sequence DESC);
                 CREATE TABLE background_jobs (id TEXT PRIMARY KEY NOT NULL,
                     project_id TEXT NOT NULL REFERENCES projects(id), kind TEXT NOT NULL,
                     status TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
                     message TEXT NOT NULL);
                 CREATE INDEX background_jobs_project ON background_jobs(project_id, started_at DESC, id);
                 CREATE TABLE settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
                 PRAGMA user_version = 1;",
            )?;
            connection.execute(
                "INSERT INTO projects VALUES (?1, ?2, ?3, ?4)",
                params![
                    project.id,
                    project.name,
                    project.path.to_str(),
                    project.created_at
                ],
            )?;
            connection.execute("INSERT INTO project_scans(project_id, scanned_at, report_json) VALUES (?1, ?2, ?3)",
                params![project.id, scan.scanned_at, serde_json::to_string(&scan)?])?;
            connection.execute("INSERT INTO background_jobs VALUES ('legacy-job', ?1, 'scan', 'running', 'earlier', NULL, 'Existing job')", [&project.id])?;
            connection.execute("INSERT INTO settings VALUES ('theme', 'dark')", [])?;
        }
        let store = Store::open(&database)?;
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            SCHEMA_VERSION
        );
        assert_eq!(store.project(&project.id)?, Some(project.clone()));
        assert_eq!(store.latest_scan(&project.id)?, Some(scan));
        assert_eq!(store.get_setting("theme")?, Some("dark".into()));
        assert_eq!(store.recover_interrupted_jobs()?, 0);
        let jobs = store.jobs(&project.id, 10)?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "legacy-job");
        assert_eq!(jobs[0].owner_pid, None);
        assert_eq!(jobs[0].status, "running");
        // Opening a migrated database again is idempotent.
        assert_eq!(Store::open(&database)?.jobs(&project.id, 10)?, jobs);
        Ok(())
    }

    #[test]
    fn legacy_serialized_jobs_default_to_an_unknown_owner() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        let legacy = job(&project, "legacy-json", "running");
        let mut json = serde_json::to_value(&legacy)?;
        json.as_object_mut()
            .context("Expected job object")?
            .remove("owner_pid");
        let decoded: JobRecord = serde_json::from_value(json)?;
        assert_eq!(decoded, legacy);
        Ok(())
    }

    #[test]
    fn job_updates_preserve_identity_and_enforce_project_foreign_keys() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        let mut entry = job(&project, "scan-one", "running");
        store.save_job(&entry)?;
        entry.status = "completed".into();
        entry.finished_at = Some(timestamp());
        store.save_job(&entry)?;
        assert_eq!(store.jobs(&project.id, 10)?, vec![entry.clone()]);
        let mut different_owner = entry.clone();
        different_owner.owner_pid = Some(std::process::id());
        assert!(store.save_job(&different_owner).is_err());
        assert_eq!(store.jobs(&project.id, 10)?, vec![entry.clone()]);
        entry.kind = "different-operation".into();
        assert!(store.save_job(&entry).is_err());
        entry.id = "new-id".into();
        entry.project_id = "missing-project".into();
        assert!(store.save_job(&entry).is_err());
        assert_eq!(store.jobs(&project.id, 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn future_schema_is_rejected_without_changing_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("future.sqlite");
        {
            let connection = Connection::open(&database)?;
            connection.pragma_update(None, "user_version", 999)?;
        }
        assert!(Store::open(&database).is_err());
        let connection = Connection::open(&database)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 999);
        Ok(())
    }

    #[test]
    fn failed_initial_migration_rolls_back_without_altering_existing_data() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("inconsistent.sqlite");
        {
            let connection = Connection::open(&database)?;
            connection.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO settings VALUES ('preserved', 'original');",
            )?;
        }
        assert!(Store::open(&database).is_err());
        let connection = Connection::open(&database)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, 0);
        let project_tables: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(project_tables, 0);
        let value: String = connection.query_row(
            "SELECT value FROM settings WHERE key = 'preserved'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(value, "original");
        Ok(())
    }

    #[test]
    fn malformed_stored_reports_are_reported_and_settings_are_upserted() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open_memory()?;
        let project = store.add_project(directory.path())?;
        store.connection.execute("INSERT INTO project_scans(project_id, scanned_at, report_json) VALUES (?1, 'now', '{')", [&project.id])?;
        assert!(store.latest_scan(&project.id).is_err());
        assert!(store.scan_history(&project.id, 10).is_err());
        assert!(store.get_setting("theme")?.is_none());
        store.set_setting("theme", "light")?;
        store.set_setting("theme", "dark")?;
        assert_eq!(store.get_setting("theme")?, Some("dark".into()));
        assert!(store.set_setting("", "value").is_err());
        Ok(())
    }
}
