//! Shared application operations. Desktop and CLI call this service rather than
//! duplicating discovery or persistence in their presentation layers.
use crate::{JobRecord, Project, ScanReport, Store, scan_project, stable_id, timestamp};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Core {
    data_dir: PathBuf,
}

impl Core {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir).with_context(|| {
            format!(
                "Cannot create Verifact data directory {}",
                data_dir.display()
            )
        })?;
        Store::open(data_dir.join("verifact.sqlite3"))?;
        Ok(Self { data_dir })
    }

    pub fn store(&self) -> Result<Store> {
        Store::open(self.data_dir.join("verifact.sqlite3"))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn add_project(&self, path: impl AsRef<Path>) -> Result<Project> {
        self.store()?.add_project(path)
    }

    pub fn configure_project(
        &self,
        id: &str,
        name: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Project> {
        self.store()?.configure_project(id, name, base_url)
    }

    pub fn project_base_url(&self, id: &str) -> Result<Option<String>> {
        self.store()?.project_base_url(id)
    }

    pub fn scan(&self, project: &Project) -> Result<ScanReport> {
        let store = self.store()?;
        let started_at = timestamp();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        let mut job = JobRecord {
            id: stable_id(&[&project.id, &nonce, "scan"]),
            project_id: project.id.clone(),
            kind: "project_scan".into(),
            status: "running".into(),
            started_at,
            finished_at: None,
            message: "Inspecting project files without executing project code".into(),
            owner_pid: Some(std::process::id()),
        };
        store.save_job(&job)?;
        let result = self
            .scan_profile(&project.id)
            .and_then(|profile| scan_project(project, &profile.options()))
            .and_then(|report| {
                store.save_scan(&report)?;
                Ok(report)
            });
        job.finished_at = Some(timestamp());
        match &result {
            Ok(report) => {
                job.status = "completed".into();
                job.message = format!(
                    "{} framework configurations; {} candidate test files",
                    report.frameworks.len(),
                    report.candidates.len()
                );
            }
            Err(error) => {
                job.status = "failed".into();
                job.message = format!("{error:#}");
            }
        }
        store
            .save_job(&job)
            .context("Could not persist final scan job state")?;
        result
    }
}

pub fn default_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("VERIFACT_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Set VERIFACT_DATA_DIR: the user home directory is unavailable")?;
    #[cfg(target_os = "macos")]
    {
        Ok(home.join("Library/Application Support/Verifact"))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join("Verifact"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("verifact"))
    }
}
