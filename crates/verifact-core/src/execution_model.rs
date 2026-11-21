use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Preparing,
    Running,
    Passed,
    Failed,
    Skipped,
    Cancelled,
    TimedOut,
    Blocked,
    InfrastructureError,
}
impl RunStatus {
    pub fn is_final(self) -> bool {
        !matches!(self, Self::Queued | Self::Preparing | Self::Running)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Blocked => "blocked",
            Self::InfrastructureError => "infrastructure_error",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReporterFormat {
    NodeTap,
    JestJson,
    VitestJson,
    PhpunitJunit,
    PlaywrightJson,
    MochaJson,
    CypressJunit,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub report_path: PathBuf,
    pub format: ReporterFormat,
    pub framework: String,
    pub candidate_ids: Vec<String>,
    pub runtime_versions: BTreeMap<String, String>,
    pub environment: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunOptions {
    pub timeout_ms: u64,
    pub max_concurrency: usize,
    pub max_output_bytes: u64,
    pub actor: String,
}
impl Default for RunOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            max_concurrency: 2,
            max_output_bytes: 1_048_576,
            actor: "human".into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub id: String,
    pub project_id: String,
    pub project_path: PathBuf,
    pub candidate_ids: Vec<String>,
    pub commands: Vec<AdapterCommand>,
    pub options: RunOptions,
    pub planned_at: String,
    pub revision: Option<String>,
    /// None means that working-tree state has not been measured.
    pub working_tree: Option<String>,
    #[serde(default)]
    pub source_fingerprint: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeTestResult {
    pub candidate_id: Option<String>,
    pub native_id: String,
    pub name: String,
    pub status: RunStatus,
    pub duration_ms: Option<u64>,
    pub assertions: Option<u64>,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub kind: String,
    /// Application-data-relative managed artifact path.
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub project_id: String,
    pub plan: ExecutionPlan,
    pub status: RunStatus,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub results: Vec<NativeTestResult>,
    pub artifacts: Vec<Artifact>,
    pub message: Option<String>,
    pub owner_pid: Option<u32>,
    pub cancel_requested: bool,
    pub output_truncated: bool,
    pub revision: Option<String>,
    pub working_tree: Option<String>,
    #[serde(default)]
    pub source_state: Option<crate::SourceState>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunOutput {
    pub run_id: String,
    pub offset: u64,
    pub next_offset: u64,
    pub text: String,
    pub complete: bool,
    pub truncated: bool,
}
