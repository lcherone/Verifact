use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Framework {
    pub id: String,
    pub package_path: PathBuf,
    pub name: String,
    pub language: String,
    pub evidence: Vec<String>,
    pub configurations: Vec<PathBuf>,
    pub suggested_command: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestCandidate {
    pub id: String,
    pub framework_id: String,
    pub file: PathBuf,
    pub display_name: String,
    /// Passive discovery identifies files, not confirmed executable test cases.
    pub discovery_kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanReport {
    pub project_id: String,
    pub scanned_at: String,
    pub frameworks: Vec<Framework>,
    pub candidates: Vec<TestCandidate>,
    pub warnings: Vec<ScanWarning>,
    pub files_inspected: usize,
    pub revision: Option<String>,
    pub working_tree: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: String,
    /// Missing on legacy records. An unknown owner is never assumed dead.
    #[serde(default)]
    pub owner_pid: Option<u32>,
}
