use crate::{JobRecord, RunRecord, RunStatus};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceState {
    pub revision: Option<String>,
    pub fingerprint: Option<String>,
    pub complete: bool,
    pub dirty: Option<bool>,
    pub files: usize,
    pub bytes: u64,
    pub captured_at: String,
    pub limitations: Vec<String>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactOrigin {
    StaticAnalysis,
    TestDiscovery,
    BrowserObservation,
    #[default]
    ManualAnnotation,
    ApprovedRequirement,
    AiInference,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvidenceRef {
    pub kind: String,
    pub reference: String,
    pub description: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KnowledgeFact {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub subject: String,
    pub label: String,
    pub description: String,
    pub origin: FactOrigin,
    pub evidence: Vec<EvidenceRef>,
    pub confidence: f64,
    pub revision: Option<String>,
    pub approved: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KnowledgeRelationship {
    pub id: String,
    pub project_id: String,
    pub from_id: String,
    pub to_id: String,
    pub kind: String,
    pub origin: FactOrigin,
    pub evidence: Vec<EvidenceRef>,
    pub confidence: f64,
    pub revision: Option<String>,
    pub approved: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecommendationReason {
    pub kind: String,
    pub evidence: String,
    pub confidence: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecommendedTest {
    pub candidate_id: String,
    pub name: String,
    pub file: PathBuf,
    pub confidence: f64,
    pub reasons: Vec<RecommendationReason>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecommendationReport {
    pub project_id: String,
    pub changed_files: Vec<PathBuf>,
    pub tests: Vec<RecommendedTest>,
    pub exhaustive: bool,
    pub limitations: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageDimension {
    pub available: bool,
    pub covered: Option<usize>,
    pub total: Option<usize>,
    pub description: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageReport {
    pub project_id: String,
    pub source_code: CoverageDimension,
    pub file_execution: CoverageDimension,
    pub feature_links: CoverageDimension,
    pub journey_links: CoverageDimension,
    pub limitations: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationPolicy {
    pub required_candidate_ids: Vec<String>,
    pub allow_skipped: bool,
    pub max_age_seconds: u64,
    pub require_current_source: bool,
}
impl Default for VerificationPolicy {
    fn default() -> Self {
        Self {
            required_candidate_ids: vec![],
            allow_skipped: false,
            max_age_seconds: 3600,
            require_current_source: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Verified,
    Failed,
    Incomplete,
    Stale,
    Blocked,
    NotEvaluated,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationCheck {
    pub candidate_id: String,
    pub name: String,
    pub status: VerificationStatus,
    pub run_id: Option<String>,
    pub run_status: Option<RunStatus>,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationReport {
    pub project_id: String,
    pub status: VerificationStatus,
    pub policy: Option<VerificationPolicy>,
    pub source: SourceState,
    pub checks: Vec<VerificationCheck>,
    pub warnings: Vec<String>,
    pub generated_at: String,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    #[default]
    Medium,
    High,
    Critical,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    #[default]
    Discovered,
    Investigating,
    Confirmed,
    Dismissed,
    Resolved,
    RegressionCovered,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Finding {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub severity: Severity,
    pub origin: FactOrigin,
    pub status: FindingStatus,
    pub evidence: Vec<EvidenceRef>,
    pub related_candidate_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Cancelled,
    Blocked,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct InvestigationBudget {
    pub max_browser_actions: u32,
    pub max_duration_ms: u64,
    pub max_tokens: u64,
    pub max_network_requests: u32,
    pub max_retries: u32,
}
impl Default for InvestigationBudget {
    fn default() -> Self {
        Self {
            max_browser_actions: 50,
            max_duration_ms: 300_000,
            max_tokens: 10_000,
            max_network_requests: 200,
            max_retries: 0,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Investigation {
    pub id: String,
    pub project_id: String,
    pub finding_id: Option<String>,
    pub target: String,
    pub reason: String,
    pub hypothesis: String,
    pub origin: FactOrigin,
    pub status: InvestigationStatus,
    pub budget: InvestigationBudget,
    pub evidence: Vec<EvidenceRef>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticError {
    pub operation_id: String,
    pub project_id: String,
    pub message: String,
    pub timestamp: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostics {
    pub schema_version: i64,
    pub active_runs: Vec<RunRecord>,
    pub active_jobs: Vec<JobRecord>,
    pub recent_errors: Vec<DiagnosticError>,
    pub runtime_versions: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}
