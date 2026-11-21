//! Deterministic recommendations, explicitly sourced knowledge and evidence gates.
//! This module schedules no AI/browser actions and never promotes hypotheses into requirements.
use crate::{execution::redact, store::IntelligenceTable, *};
use anyhow::{Context, Result, ensure};
use std::io::Read;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

impl Core {
    fn intelligence_project(&self, id: &str) -> Result<Project> {
        self.store()?.project(id)?.context("Unknown project")
    }

    pub fn facts(&self, project_id: &str, limit: usize) -> Result<Vec<KnowledgeFact>> {
        self.intelligence_project(project_id)?;
        self.store()?
            .intelligence_list(IntelligenceTable::Facts, project_id, limit)
    }
    pub fn save_fact(&self, mut fact: KnowledgeFact) -> Result<KnowledgeFact> {
        self.intelligence_project(&fact.project_id)?;
        validate_text(&fact.kind, "Fact kind", 128)?;
        validate_text(&fact.label, "Fact label", 512)?;
        validate_confidence(fact.confidence)?;
        validate_evidence(&fact.evidence)?;
        fact.id = identity(&fact.id, &fact.project_id, "fact")?;
        fact.approved = false;
        self.store()?
            .intelligence_save(IntelligenceTable::Facts, scrub(fact)?)
    }
    pub fn approve_fact(
        &self,
        project_id: &str,
        id: &str,
        approved: bool,
    ) -> Result<KnowledgeFact> {
        self.intelligence_project(project_id)?;
        let store = self.store()?;
        let mut fact: KnowledgeFact = store
            .intelligence_get(IntelligenceTable::Facts, project_id, id)?
            .context("Unknown fact")?;
        fact.approved = approved;
        store.intelligence_save(IntelligenceTable::Facts, fact)
    }
    pub fn relationships(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeRelationship>> {
        self.intelligence_project(project_id)?;
        self.store()?
            .intelligence_list(IntelligenceTable::Relationships, project_id, limit)
    }
    pub fn save_relationship(
        &self,
        mut relationship: KnowledgeRelationship,
    ) -> Result<KnowledgeRelationship> {
        self.intelligence_project(&relationship.project_id)?;
        validate_text(&relationship.kind, "Relationship kind", 128)?;
        validate_confidence(relationship.confidence)?;
        validate_evidence(&relationship.evidence)?;
        ensure!(
            relationship.from_id != relationship.to_id,
            "A relationship must connect distinct facts"
        );
        relationship.id = identity(&relationship.id, &relationship.project_id, "relationship")?;
        relationship.approved = false;
        self.store()?
            .intelligence_save(IntelligenceTable::Relationships, scrub(relationship)?)
    }
    pub fn approve_relationship(
        &self,
        project_id: &str,
        id: &str,
        approved: bool,
    ) -> Result<KnowledgeRelationship> {
        self.intelligence_project(project_id)?;
        let store = self.store()?;
        let mut relationship: KnowledgeRelationship = store
            .intelligence_get(IntelligenceTable::Relationships, project_id, id)?
            .context("Unknown relationship")?;
        if approved {
            for endpoint in [&relationship.from_id, &relationship.to_id] {
                let fact: KnowledgeFact = store
                    .intelligence_get(IntelligenceTable::Facts, project_id, endpoint)?
                    .context("Unknown relationship endpoint")?;
                ensure!(fact.approved, "Approve relationship endpoint facts first");
            }
        }
        relationship.approved = approved;
        store.intelligence_save(IntelligenceTable::Relationships, relationship)
    }
    pub fn findings(&self, project_id: &str, limit: usize) -> Result<Vec<Finding>> {
        self.intelligence_project(project_id)?;
        self.store()?
            .intelligence_list(IntelligenceTable::Findings, project_id, limit)
    }
    pub fn save_finding(&self, mut finding: Finding) -> Result<Finding> {
        self.intelligence_project(&finding.project_id)?;
        validate_text(&finding.title, "Finding title", 512)?;
        validate_evidence(&finding.evidence)?;
        ensure!(
            finding.related_candidate_ids.len() <= 1000,
            "Too many linked test candidates"
        );
        if !finding.related_candidate_ids.is_empty() {
            let report = self
                .store()?
                .latest_scan(&finding.project_id)?
                .context("Scan the project before linking candidates")?;
            ensure!(
                finding.related_candidate_ids.iter().all(|id| report
                    .candidates
                    .iter()
                    .any(|candidate| &candidate.id == id)),
                "Finding links an unknown candidate"
            );
        }
        finding.id = identity(&finding.id, &finding.project_id, "finding")?;
        self.store()?
            .intelligence_save(IntelligenceTable::Findings, scrub(finding)?)
    }
    pub fn investigations(&self, project_id: &str, limit: usize) -> Result<Vec<Investigation>> {
        self.intelligence_project(project_id)?;
        self.store()?
            .intelligence_list(IntelligenceTable::Investigations, project_id, limit)
    }
    pub fn save_investigation(&self, mut investigation: Investigation) -> Result<Investigation> {
        self.intelligence_project(&investigation.project_id)?;
        validate_text(&investigation.target, "Investigation target", 512)?;
        validate_text(&investigation.reason, "Investigation reason", 4096)?;
        validate_evidence(&investigation.evidence)?;
        let budget = &investigation.budget;
        ensure!(
            (100..=3_600_000).contains(&budget.max_duration_ms)
                && budget.max_browser_actions <= 1000
                && budget.max_network_requests <= 10_000
                && budget.max_tokens <= 1_000_000
                && budget.max_retries <= 10,
            "Investigation budget exceeds supported bounds"
        );
        investigation.id = identity(
            &investigation.id,
            &investigation.project_id,
            "investigation",
        )?;
        self.store()?
            .intelligence_save(IntelligenceTable::Investigations, scrub(investigation)?)
    }
    pub fn cancel_investigation(&self, id: &str) -> Result<Investigation> {
        let store = self.store()?;
        let mut investigation: Investigation = store
            .intelligence_find(IntelligenceTable::Investigations, id)?
            .context("Unknown investigation")?;
        if matches!(
            investigation.status,
            InvestigationStatus::Completed | InvestigationStatus::Cancelled
        ) {
            return Ok(investigation);
        }
        investigation.status = InvestigationStatus::Cancelled;
        store.intelligence_save(IntelligenceTable::Investigations, investigation)
    }
    pub fn recommend_tests(
        &self,
        project_id: &str,
        changed_files: &[PathBuf],
    ) -> Result<RecommendationReport> {
        let project = self.intelligence_project(project_id)?;
        ensure!(
            changed_files.len() <= 500,
            "At most 500 changed paths per recommendation request"
        );
        let changed: Vec<PathBuf> = changed_files
            .iter()
            .map(|path| relative_path(path, &project.path))
            .collect::<Result<_>>()?;
        let report = self
            .store()?
            .latest_scan(project_id)?
            .context("Scan the project before requesting test recommendations")?;
        let imports = regex::Regex::new(
            r#"(?:\bfrom\s+|\brequire\s*\(\s*|\bimport\s*\(\s*|\bimport\s+)["']([^"']+)["']"#,
        )?;
        let mut tests = Vec::new();
        let mut read_bytes = 0usize;
        let mut limitations=vec!["Recommendations use exact candidate paths, literal relative imports and file-name similarity. Dynamic dependencies, aliases, runtime behavior and full change impact are not resolved; this list is not exhaustive.".into()];
        for candidate in report.candidates.iter().take(20_000) {
            if tests.len() >= 1000 {
                limitations.push("Recommendation output reached its 1000-candidate limit; additional matches were omitted.".into());
                break;
            }
            let mut reasons = Vec::new();
            if changed.contains(&candidate.file) {
                reasons.push(RecommendationReason {
                    kind: "exact_test_file".into(),
                    evidence: candidate.file.display().to_string(),
                    confidence: 1.0,
                });
            }
            for file in &changed {
                if reasons.len() >= 4 {
                    break;
                }
                if &candidate.file != file
                    && file_key(file) == file_key(&candidate.file)
                    && !file_key(file).is_empty()
                {
                    reasons.push(RecommendationReason {
                        kind: "file_name_similarity".into(),
                        evidence: format!(
                            "{} shares a normalized file name with {}",
                            candidate.file.display(),
                            file.display()
                        ),
                        confidence: 0.45,
                    });
                }
            }
            if read_bytes < 8 * 1024 * 1024 {
                let path = project.path.join(&candidate.file);
                if fs::symlink_metadata(&path).is_ok_and(|metadata| {
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() <= 128 * 1024
                }) && fs::canonicalize(&path).is_ok_and(|path| path.starts_with(&project.path))
                    && let Ok(text) = bounded_source_text(&path)
                {
                    read_bytes += text.len();
                    for capture in imports.captures_iter(&text) {
                        if reasons.len() >= 8 {
                            break;
                        }
                        let import = &capture[1];
                        if !import.starts_with('.') {
                            continue;
                        }
                        let Ok(target) = normalize_relative(
                            &candidate
                                .file
                                .parent()
                                .unwrap_or(Path::new(""))
                                .join(import),
                        ) else {
                            continue;
                        };
                        for changed in &changed {
                            if reasons.len() >= 8 {
                                break;
                            }
                            if same_import_target(&target, changed) {
                                reasons.push(RecommendationReason {
                                    kind: "literal_relative_import".into(),
                                    evidence: format!(
                                        "{} imports {import}",
                                        candidate.file.display()
                                    ),
                                    confidence: 0.9,
                                });
                            }
                        }
                    }
                }
            }
            if !reasons.is_empty() {
                tests.push(RecommendedTest {
                    candidate_id: candidate.id.clone(),
                    name: candidate.display_name.clone(),
                    file: candidate.file.clone(),
                    confidence: reasons
                        .iter()
                        .map(|reason| reason.confidence)
                        .fold(0.0, f64::max),
                    reasons,
                });
            }
        }
        if read_bytes >= 8 * 1024 * 1024 || report.candidates.len() > 20_000 {
            limitations.push("The bounded source-inspection budget was reached; remaining candidates were not inspected for imports.".into());
        }
        tests.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then(left.candidate_id.cmp(&right.candidate_id))
        });
        Ok(RecommendationReport {
            project_id: project_id.into(),
            changed_files: changed,
            tests,
            exhaustive: false,
            limitations,
        })
    }
    pub fn coverage(&self, project_id: &str) -> Result<CoverageReport> {
        self.intelligence_project(project_id)?;
        let store = self.store()?;
        let report = store.latest_scan(project_id)?;
        let candidates = report
            .as_ref()
            .map(|report| report.candidates.as_slice())
            .unwrap_or_default();
        let history = store.run_history(project_id, 1000)?;
        let mut executed = HashSet::new();
        for run in &history {
            for result in &run.results {
                if run.status.is_final()
                    && matches!(result.status, RunStatus::Passed | RunStatus::Failed)
                    && let Some(id) = &result.candidate_id
                {
                    executed.insert(id.clone());
                }
            }
        }
        let facts = self.facts(project_id, 1000)?;
        let relationships = self.relationships(project_id, 1000)?;
        let approved: HashMap<_, _> = facts
            .iter()
            .filter(|fact| fact.approved)
            .map(|fact| (fact.id.as_str(), fact))
            .collect();
        let linked = |kind: &str| {
            let features: Vec<_> = facts
                .iter()
                .filter(|fact| fact.approved && fact.kind == kind)
                .collect();
            let covered = features
                .iter()
                .filter(|feature| {
                    relationships.iter().any(|relationship| {
                        relationship.approved
                            && relationship.kind == "verifies"
                            && relationship.to_id == feature.id
                            && approved
                                .get(relationship.from_id.as_str())
                                .is_some_and(|test| {
                                    test.kind == "test"
                                        && candidates
                                            .iter()
                                            .any(|candidate| candidate.id == test.subject)
                                })
                    })
                })
                .count();
            CoverageDimension {
                available: !features.is_empty(),
                covered: (!features.is_empty()).then_some(covered),
                total: (!features.is_empty()).then_some(features.len()),
                description: format!(
                    "Approved manually curated {kind} facts linked to current test-file candidates; links do not prove execution or exhaustive coverage."
                ),
            }
        };
        Ok(CoverageReport {
            project_id: project_id.into(),
            source_code: CoverageDimension {
                available: false,
                covered: None,
                total: None,
                description: "No native source-code coverage report has been imported.".into(),
            },
            file_execution: CoverageDimension {
                available: report.is_some(),
                covered: report.as_ref().map(|_| candidates.iter().filter(|candidate| executed.contains(&candidate.id)).count()),
                total: report.as_ref().map(|_| candidates.len()),
                description: "Candidate files with executed passing/failing native cases in the last 1000 run records; skips alone do not count. This is not source or feature coverage.".into(),
            },
            feature_links: linked("feature"),
            journey_links: linked("journey"),
            limitations: vec!["Dimensions remain separate. Knowledge queries are capped at 1000 records; historical execution may be stale and does not establish current verification.".into()],
        })
    }
    pub fn set_verification_policy(
        &self,
        project_id: &str,
        policy: VerificationPolicy,
    ) -> Result<VerificationPolicy> {
        self.intelligence_project(project_id)?;
        ensure!(
            (1..=31_536_000).contains(&policy.max_age_seconds),
            "Policy maximum age must be between one second and one year"
        );
        ensure!(
            policy.required_candidate_ids.len() <= 1000,
            "At most 1000 required candidates are supported"
        );
        let unique: HashSet<_> = policy.required_candidate_ids.iter().collect();
        ensure!(
            unique.len() == policy.required_candidate_ids.len(),
            "Policy repeats a candidate"
        );
        if !unique.is_empty() {
            let report = self
                .store()?
                .latest_scan(project_id)?
                .context("Scan the project before setting required candidates")?;
            ensure!(
                unique.iter().all(|id| report
                    .candidates
                    .iter()
                    .any(|candidate| &&candidate.id == id)),
                "Policy references an unknown candidate"
            );
        }
        self.store()?.set_setting(
            &format!("project:{project_id}:verification_policy"),
            &serde_json::to_string(&policy)?,
        )?;
        Ok(policy)
    }
    pub fn verification_status(&self, project_id: &str) -> Result<VerificationReport> {
        self.verification_report(project_id)
    }
    pub fn verification_report(&self, project_id: &str) -> Result<VerificationReport> {
        self.intelligence_project(project_id)?;
        let store = self.store()?;
        let policy: Option<VerificationPolicy> = store
            .get_setting(&format!("project:{project_id}:verification_policy"))?
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        let source = self.source_state(project_id)?;
        let mut result = VerificationReport {
            project_id: project_id.into(),
            status: VerificationStatus::NotEvaluated,
            policy: policy.clone(),
            source,
            checks: vec![],
            warnings: vec!["This gate evaluates recorded native cases for required candidate files. It does not establish complete feature correctness or independent attestation.".into()],
            generated_at: timestamp(),
        };
        let Some(policy) = policy else {
            return Ok(result);
        };
        let report = store.latest_scan(project_id)?;
        let candidates = report
            .as_ref()
            .map(|report| report.candidates.as_slice())
            .unwrap_or_default();
        let required = if policy.required_candidate_ids.is_empty() {
            candidates
                .iter()
                .map(|candidate| candidate.id.clone())
                .collect::<Vec<_>>()
        } else {
            policy.required_candidate_ids.clone()
        };
        let history = self.run_history(project_id, 1000)?;
        for id in required {
            let candidate = candidates.iter().find(|candidate| candidate.id == id);
            let mut check = VerificationCheck {
                candidate_id: id.clone(),
                name: candidate
                    .map(|candidate| candidate.display_name.clone())
                    .unwrap_or_else(|| id.clone()),
                status: VerificationStatus::Incomplete,
                run_id: None,
                run_status: None,
                message: "No recorded execution evidence in the last 1000 runs".into(),
            };
            if candidate.is_none() {
                check.status = VerificationStatus::Blocked;
                check.message = "Required candidate is no longer in the current catalogue".into();
            } else if let Some(run) = history
                .iter()
                .find(|run| run.plan.candidate_ids.contains(&id))
            {
                evaluate_check(&mut check, run, &policy, &result.source);
            }
            result.checks.push(check);
        }
        result.status = if result.checks.is_empty() {
            VerificationStatus::Incomplete
        } else {
            [
                VerificationStatus::Failed,
                VerificationStatus::Blocked,
                VerificationStatus::Stale,
                VerificationStatus::Incomplete,
            ]
            .into_iter()
            .find(|status| result.checks.iter().any(|check| check.status == *status))
            .unwrap_or(VerificationStatus::Verified)
        };
        if !result.source.complete && result.status == VerificationStatus::Verified {
            result.status = VerificationStatus::Incomplete;
        }
        if !result.source.complete {
            result.warnings.push("Current source identity is unavailable or incomplete; current verification cannot be established.".into());
        }
        if !policy.require_current_source {
            result.warnings.push("This policy permits historical source fingerprints; results may not describe the current source.".into());
        }
        Ok(result)
    }
    pub fn diagnostics(&self) -> Result<Diagnostics> {
        let store = self.store()?;
        store.recover_interrupted_runs()?;
        let recent = store.recent_runs(20)?;
        let mut runtime_versions = BTreeMap::from([
            ("verifact_core".into(), env!("CARGO_PKG_VERSION").into()),
            (
                "platform".into(),
                format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            ),
        ]);
        let mut recent_errors = Vec::new();
        for run in recent {
            for command in &run.plan.commands {
                for (runtime, version) in &command.runtime_versions {
                    runtime_versions
                        .entry(format!("last_observed/{runtime}"))
                        .or_insert_with(|| version.clone());
                }
            }
            if run.status == RunStatus::InfrastructureError {
                recent_errors.push(DiagnosticError {
                    operation_id: run.id,
                    project_id: run.project_id,
                    message: run
                        .message
                        .unwrap_or_else(|| "Native runner reported an infrastructure error".into()),
                    timestamp: run.finished_at.unwrap_or(run.queued_at),
                });
            }
        }
        Ok(Diagnostics {
            schema_version: store.schema_version()?,
            active_runs: store.active_runs()?,
            active_jobs: store.active_jobs()?,
            recent_errors,
            runtime_versions,
            limitations: vec!["Recent errors cover the last 20 execution records. Runtime versions are labeled historical observations; browser/AI/remote runner health is not probed by this core endpoint.".into()],
        })
    }
}

fn evaluate_check(
    check: &mut VerificationCheck,
    run: &RunRecord,
    policy: &VerificationPolicy,
    current: &SourceState,
) {
    check.run_id = Some(run.id.clone());
    check.run_status = Some(run.status);
    if !run.status.is_final() {
        check.message = "Latest requested execution is not finalized".into();
        return;
    }
    let cases: Vec<_> = run
        .results
        .iter()
        .filter(|result| result.candidate_id.as_deref() == Some(check.candidate_id.as_str()))
        .collect();
    if cases.iter().any(|case| case.status == RunStatus::Failed) {
        check.status = VerificationStatus::Failed;
        check.message = "A native assertion/test failed".into();
        return;
    }
    if cases.iter().any(|case| {
        matches!(
            case.status,
            RunStatus::Blocked | RunStatus::InfrastructureError
        )
    }) || matches!(
        run.status,
        RunStatus::InfrastructureError | RunStatus::Blocked | RunStatus::TimedOut
    ) && cases.is_empty()
    {
        check.status = VerificationStatus::Blocked;
        check.message = "Runner/environment evidence is blocked or incomplete".into();
        return;
    }
    if !cases.iter().any(|case| case.status == RunStatus::Passed)
        || cases
            .iter()
            .any(|case| !matches!(case.status, RunStatus::Passed | RunStatus::Skipped))
    {
        check.message="No complete passing native case evidence; skipped-only and cancelled execution do not verify a file".into();
        return;
    }
    if !policy.allow_skipped && cases.iter().any(|case| case.status == RunStatus::Skipped) {
        check.message = "Required cases were skipped and policy forbids skips".into();
        return;
    }
    let Some(source) = run
        .source_state
        .as_ref()
        .filter(|source| source.complete && source.fingerprint.is_some())
    else {
        check.message = "Execution source fingerprint is unavailable".into();
        return;
    };
    if !current.complete {
        check.message = "Current source fingerprint is unavailable".into();
        return;
    }
    if policy.require_current_source
        && (source.fingerprint != current.fingerprint || source.revision != current.revision)
    {
        check.status = VerificationStatus::Stale;
        check.message = "Source contents/revision changed since execution".into();
        return;
    }
    let started = run
        .started_at
        .as_deref()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    let Some(started) = started else {
        check.message = "Execution time is unavailable".into();
        return;
    };
    let age = chrono::Utc::now()
        .signed_duration_since(started)
        .num_seconds();
    if age < 0 {
        check.message = "Execution timestamp is in the future".into();
        return;
    }
    if age as u64 > policy.max_age_seconds {
        check.status = VerificationStatus::Stale;
        check.message = "Execution exceeds the policy maximum age".into();
        return;
    }
    check.status = VerificationStatus::Verified;
    check.message = "Recorded native cases satisfy this candidate's configured policy".into();
}
fn identity(id: &str, project: &str, kind: &str) -> Result<String> {
    if !id.is_empty() {
        ensure!(
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Intelligence identity must be a 64-character hexadecimal ID"
        );
        return Ok(id.into());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    Ok(stable_id(&[project, kind, &nonce]))
}
fn validate_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= maximum
            && !value
                .chars()
                .any(|character| character.is_control() && character != '\n' && character != '\t'),
        "{label} must be nonempty and at most {maximum} bytes"
    );
    Ok(())
}
fn validate_confidence(confidence: f64) -> Result<()> {
    ensure!(
        confidence.is_finite() && (0.0..=1.0).contains(&confidence),
        "Confidence must be a finite value between zero and one"
    );
    Ok(())
}
fn validate_evidence(evidence: &[EvidenceRef]) -> Result<()> {
    ensure!(
        evidence.len() <= 64,
        "At most 64 evidence references are supported"
    );
    for item in evidence {
        validate_text(&item.kind, "Evidence kind", 128)?;
        validate_text(&item.reference, "Evidence reference", 4096)?;
    }
    Ok(())
}
fn scrub<T: serde::Serialize + serde::de::DeserializeOwned>(value: T) -> Result<T> {
    let json = serde_json::to_string(&value)?;
    ensure!(json.len() <= 65_536, "Intelligence record exceeds 64 KiB");
    Ok(serde_json::from_str(&redact(&json))?)
}
fn relative_path(path: &Path, root: &Path) -> Result<PathBuf> {
    ensure!(
        path.as_os_str().len() <= 4096,
        "Changed path exceeds 4096 bytes"
    );
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .context("Changed path is outside the project")?
    } else {
        path
    };
    normalize_relative(relative)
}
fn normalize_relative(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => ensure!(normalized.pop(), "Path escapes the project"),
            _ => anyhow::bail!("Project-relative path required"),
        }
    }
    ensure!(
        !normalized.as_os_str().is_empty(),
        "A file path is required"
    );
    Ok(normalized)
}
fn file_key(path: &Path) -> String {
    let mut name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    for suffix in [".test", ".spec", "test", "tests", "spec"] {
        if name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}
fn same_import_target(import: &Path, changed: &Path) -> bool {
    import == changed
        || import.extension().is_none()
            && ["js", "mjs", "cjs", "jsx", "ts", "tsx", "php", "vue"]
                .iter()
                .any(|extension| import.with_extension(extension) == changed)
        || ["index.js", "index.ts", "index.tsx"]
            .iter()
            .any(|file| import.join(file) == changed)
}

fn bounded_source_text(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Candidate must be a regular file"
    );
    let mut text = String::new();
    file.take(128 * 1024 + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() <= 128 * 1024,
        "Candidate source exceeds inspection budget"
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> SourceState {
        SourceState {
            complete: true,
            fingerprint: Some("fixture-source".into()),
            revision: Some("fixture-revision".into()),
            ..SourceState::default()
        }
    }
    fn record() -> RunRecord {
        RunRecord {
            id: "run".into(),
            project_id: "project".into(),
            plan: ExecutionPlan {
                id: "run".into(),
                project_id: "project".into(),
                project_path: PathBuf::from("."),
                candidate_ids: vec!["candidate".into()],
                commands: vec![],
                options: RunOptions::default(),
                planned_at: timestamp(),
                revision: None,
                working_tree: None,
                source_fingerprint: None,
            },
            status: RunStatus::Passed,
            queued_at: timestamp(),
            started_at: Some(timestamp()),
            finished_at: Some(timestamp()),
            duration_ms: Some(1),
            exit_code: Some(0),
            results: vec![NativeTestResult {
                candidate_id: Some("candidate".into()),
                native_id: "native".into(),
                name: "Native case".into(),
                status: RunStatus::Passed,
                duration_ms: Some(1),
                assertions: Some(1),
                message: None,
            }],
            artifacts: vec![],
            message: None,
            owner_pid: Some(std::process::id()),
            cancel_requested: false,
            output_truncated: false,
            revision: None,
            working_tree: None,
            source_state: Some(source()),
        }
    }
    fn check(run: &RunRecord, policy: &VerificationPolicy) -> VerificationCheck {
        let mut check = VerificationCheck {
            candidate_id: "candidate".into(),
            name: "Candidate".into(),
            status: VerificationStatus::Incomplete,
            run_id: None,
            run_status: None,
            message: String::new(),
        };
        evaluate_check(&mut check, run, policy, &source());
        check
    }
    #[test]
    fn verification_rejects_expired_future_and_unknown_execution_evidence() {
        let mut run = record();
        let policy = VerificationPolicy {
            max_age_seconds: 1,
            ..VerificationPolicy::default()
        };
        assert_eq!(check(&run, &policy).status, VerificationStatus::Verified);
        run.started_at = Some((chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339());
        assert_eq!(check(&run, &policy).status, VerificationStatus::Stale);
        run.started_at = Some((chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339());
        assert_eq!(check(&run, &policy).status, VerificationStatus::Incomplete);
        run.started_at = Some(timestamp());
        run.source_state = None;
        assert_eq!(check(&run, &policy).status, VerificationStatus::Incomplete);
        run.source_state = Some(source());
        run.results.clear();
        assert_eq!(check(&run, &policy).status, VerificationStatus::Incomplete);
    }
}
