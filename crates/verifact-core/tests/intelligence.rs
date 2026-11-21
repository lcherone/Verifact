use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use verifact_core::*;

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let result = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "Fixture Git failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
fn fixture(source: &str, git_enabled: bool) -> Result<(TempDir, Core, Project)> {
    let temporary = TempDir::new()?;
    let root = temporary.path().join("project");
    fs::create_dir_all(root.join("test"))?;
    fs::create_dir(root.join("src"))?;
    fs::write(
        root.join("package.json"),
        r#"{"private":true,"scripts":{"test":"node --test"}}"#,
    )?;
    fs::write(
        root.join("src/calc.mjs"),
        "export const addition=(a,b)=>a+b;\n",
    )?;
    fs::write(root.join("test/calc.test.mjs"), source)?;
    if git_enabled {
        git(&root, &["init", "--quiet"])?;
        git(
            &root,
            &[
                "add",
                "--",
                "package.json",
                "src/calc.mjs",
                "test/calc.test.mjs",
            ],
        )?;
        git(
            &root,
            &[
                "commit",
                "--quiet",
                "--no-verify",
                "-m",
                "Synthetic fixture seed",
            ],
        )?;
    }
    let core = Core::open(temporary.path().join("data"))?;
    let project = core.add_project(root)?;
    core.scan(&project)?;
    Ok((temporary, core, project))
}
fn passing_source() -> &'static str {
    "import test from 'node:test'; import assert from 'node:assert/strict'; import {addition} from '../src/calc.mjs'; test('addition',()=>assert.equal(addition(2,2),4));"
}
fn execute(core: &Core, project: &Project) -> Result<RunRecord> {
    let run = core.start_run(
        core.plan_run(&project.id, &[], RunOptions::default())?,
        true,
    )?;
    let started = Instant::now();
    loop {
        let record = core.run(&run.id)?.context("Run")?;
        if record.status.is_final() {
            return Ok(record);
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(10),
            "Run did not complete"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn source_identity_tracks_contents_of_dirty_and_untracked_files_and_refuses_unknowns() -> Result<()>
{
    let (_temporary, core, project) = fixture(passing_source(), true)?;
    let clean = core.source_state(&project.id)?;
    assert!(clean.complete, "{clean:?}");
    assert_eq!(clean.dirty, None);
    assert!(clean.fingerprint.is_some());
    let untracked = project.path.join("new-module.mjs");
    fs::write(&untracked, "first\n")?;
    let first = core.source_state(&project.id)?;
    assert_ne!(first.fingerprint, clean.fingerprint);
    assert_eq!(first.dirty, None);
    fs::write(&untracked, "other\n")?;
    let second = core.source_state(&project.id)?;
    assert_ne!(
        first.fingerprint, second.fingerprint,
        "Same-size untracked edits must change source identity"
    );
    fs::remove_file(untracked)?;
    assert_eq!(
        core.source_state(&project.id)?.fingerprint,
        clean.fingerprint
    );
    fs::write(
        project.path.join("src/calc.mjs"),
        "export const addition=(a,b)=>a-b;\n",
    )?;
    assert_ne!(
        core.source_state(&project.id)?.fingerprint,
        clean.fingerprint
    );
    let (_other, unknown, project) = fixture(passing_source(), false)?;
    let source = unknown.source_state(&project.id)?;
    assert!(!source.complete);
    assert!(source.fingerprint.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_identity_refuses_symlinks_and_oversized_visible_files() -> Result<()> {
    let (_temporary, core, project) = fixture(passing_source(), true)?;
    std::os::unix::fs::symlink("src/calc.mjs", project.path.join("linked.mjs"))?;
    let state = core.source_state(&project.id)?;
    assert!(!state.complete);
    assert!(state.fingerprint.is_none());
    fs::remove_file(project.path.join("linked.mjs"))?;
    let file = fs::File::create(project.path.join("large.bin"))?;
    file.set_len(64 * 1024 * 1024 + 1)?;
    let state = core.source_state(&project.id)?;
    assert!(!state.complete);
    assert!(state.fingerprint.is_none());
    Ok(())
}

#[test]
fn real_execution_policy_is_verified_then_stale_after_same_path_content_change() -> Result<()> {
    let (_temporary, core, project) = fixture(passing_source(), true)?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::NotEvaluated
    );
    core.set_verification_policy(&project.id, VerificationPolicy::default())?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Incomplete
    );
    let run = execute(&core, &project)?;
    assert_eq!(run.status, RunStatus::Passed, "{run:?}");
    assert!(
        run.source_state
            .as_ref()
            .is_some_and(|source| source.complete)
    );
    let report = core.verification_report(&project.id)?;
    assert_eq!(report.status, VerificationStatus::Verified, "{report:?}");
    fs::write(
        project.path.join("src/calc.mjs"),
        "export const addition=(a,b)=>a-b;\n",
    )?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Stale
    );
    core.set_verification_policy(
        &project.id,
        VerificationPolicy {
            require_current_source: false,
            ..VerificationPolicy::default()
        },
    )?;
    let historical = core.verification_status(&project.id)?;
    assert_eq!(historical.status, VerificationStatus::Verified);
    assert!(
        historical
            .warnings
            .iter()
            .any(|warning| warning.contains("historical"))
    );
    Ok(())
}

#[test]
fn passing_native_cases_with_unknown_source_are_incomplete_and_failure_stays_failed() -> Result<()>
{
    let (_temporary, core, project) = fixture(passing_source(), false)?;
    core.set_verification_policy(&project.id, VerificationPolicy::default())?;
    assert_eq!(execute(&core, &project)?.status, RunStatus::Passed);
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Incomplete
    );
    fs::write(
        project.path.join("test/calc.test.mjs"),
        "import test from 'node:test'; import assert from 'node:assert/strict'; test('failure',()=>assert.equal(1,2));",
    )?;
    assert_eq!(execute(&core, &project)?.status, RunStatus::Failed);
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Failed
    );
    Ok(())
}

#[test]
fn skips_are_policy_controlled_but_skipped_only_files_never_verify() -> Result<()> {
    let source = "import test from 'node:test';import assert from 'node:assert/strict';test('pass',()=>assert.equal(2+2,4));test.skip('skip',()=>{});";
    let (_temporary, core, project) = fixture(source, true)?;
    core.set_verification_policy(&project.id, VerificationPolicy::default())?;
    execute(&core, &project)?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Incomplete
    );
    core.set_verification_policy(
        &project.id,
        VerificationPolicy {
            allow_skipped: true,
            ..VerificationPolicy::default()
        },
    )?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Verified
    );
    fs::write(
        project.path.join("test/calc.test.mjs"),
        "import test from 'node:test';test.skip('only skipped',()=>{});",
    )?;
    execute(&core, &project)?;
    assert_eq!(
        core.verification_status(&project.id)?.status,
        VerificationStatus::Incomplete
    );
    Ok(())
}

#[test]
fn recommendation_reasons_are_deterministic_bounded_and_not_exhaustive() -> Result<()> {
    let (_temporary, core, project) = fixture(passing_source(), false)?;
    let report = core.recommend_tests(&project.id, &[PathBuf::from("src/calc.mjs")])?;
    assert!(!report.exhaustive);
    assert_eq!(report.tests.len(), 1);
    assert!(
        report.tests[0]
            .reasons
            .iter()
            .any(|reason| reason.kind == "literal_relative_import")
    );
    assert!(
        report.tests[0]
            .reasons
            .iter()
            .any(|reason| reason.kind == "file_name_similarity")
    );
    let exact = core.recommend_tests(&project.id, &[PathBuf::from("test/calc.test.mjs")])?;
    assert_eq!(exact.tests[0].confidence, 1.0);
    assert!(
        core.recommend_tests(&project.id, &[PathBuf::from("../../outside")])
            .is_err()
    );
    assert!(
        core.recommend_tests(&project.id, &[PathBuf::from("/outside")])
            .is_err()
    );
    let unrelated = core.recommend_tests(&project.id, &[PathBuf::from("src/unrelated.mjs")])?;
    assert!(unrelated.tests.is_empty());
    assert!(!unrelated.exhaustive);
    Ok(())
}

fn fact(project: &Project, kind: &str, label: &str) -> KnowledgeFact {
    KnowledgeFact {
        project_id: project.id.clone(),
        kind: kind.into(),
        label: label.into(),
        origin: FactOrigin::AiInference,
        confidence: 0.7,
        evidence: vec![EvidenceRef {
            kind: "test".into(),
            reference: "synthetic-reference".into(),
            description: "Observation only".into(),
        }],
        ..KnowledgeFact::default()
    }
}
#[test]
fn knowledge_provenance_explicit_approval_versions_and_relationship_integrity_persist() -> Result<()>
{
    let (temporary, core, project) = fixture(passing_source(), false)?;
    let mut input = fact(&project, "feature", "Addition");
    input.approved = true;
    input.description = "password=private-value".into();
    let feature = core.save_fact(input)?;
    assert!(!feature.approved);
    assert_eq!(feature.version, 1);
    assert!(!feature.description.contains("private-value"));
    let approved = core.approve_fact(&project.id, &feature.id, true)?;
    assert!(approved.approved);
    assert_eq!(approved.version, 2);
    let candidate = core
        .store()?
        .latest_scan(&project.id)?
        .context("Scan")?
        .candidates[0]
        .id
        .clone();
    let mut test = fact(&project, "test", "Addition case");
    test.subject = candidate;
    let test = core.save_fact(test)?;
    core.approve_fact(&project.id, &test.id, true)?;
    let relationship = core.save_relationship(KnowledgeRelationship {
        project_id: project.id.clone(),
        from_id: test.id.clone(),
        to_id: feature.id.clone(),
        kind: "verifies".into(),
        origin: FactOrigin::ManualAnnotation,
        confidence: 1.0,
        ..KnowledgeRelationship::default()
    })?;
    assert!(!relationship.approved);
    core.approve_relationship(&project.id, &relationship.id, true)?;
    let coverage = core.coverage(&project.id)?;
    assert!(!coverage.source_code.available);
    assert_eq!(coverage.feature_links.covered, Some(1));
    assert_eq!(coverage.feature_links.total, Some(1));
    assert_eq!(coverage.file_execution.covered, Some(0));
    let updated = core.save_fact(approved)?;
    assert!(!updated.approved);
    assert_eq!(updated.version, 3);
    assert_eq!(core.coverage(&project.id)?.feature_links.total, None);
    let reopened = Core::open(temporary.path().join("data"))?;
    assert_eq!(reopened.facts(&project.id, 10)?.len(), 2);
    assert_eq!(reopened.relationships(&project.id, 10)?.len(), 1);
    let other_dir = temporary.path().join("other");
    fs::create_dir(&other_dir)?;
    let other = core.add_project(other_dir)?;
    let foreign = core.save_fact(fact(&other, "feature", "Other"))?;
    assert!(
        core.save_relationship(KnowledgeRelationship {
            project_id: project.id.clone(),
            from_id: test.id,
            to_id: foreign.id,
            kind: "verifies".into(),
            ..KnowledgeRelationship::default()
        })
        .is_err()
    );
    let mut invalid = fact(&project, "feature", "Invalid");
    invalid.confidence = 1.1;
    assert!(core.save_fact(invalid).is_err());
    Ok(())
}

#[test]
fn findings_and_bounded_cancelled_investigations_survive_reopen_without_running_ai() -> Result<()> {
    let (temporary, core, project) = fixture(passing_source(), false)?;
    let finding = core.save_finding(Finding {
        project_id: project.id.clone(),
        title: "Potential validation gap".into(),
        description: "Hypothesis, not a confirmed product defect".into(),
        origin: FactOrigin::AiInference,
        ..Finding::default()
    })?;
    assert_eq!(finding.status, FindingStatus::Discovered);
    let investigation = core.save_investigation(Investigation {
        project_id: project.id.clone(),
        finding_id: Some(finding.id),
        target: "Addition validation".into(),
        reason: "Review explicit boundary requirements".into(),
        hypothesis: "Inputs may need constraints".into(),
        origin: FactOrigin::AiInference,
        ..Investigation::default()
    })?;
    assert_eq!(investigation.status, InvestigationStatus::Queued);
    assert!(core.active_runs()?.is_empty());
    assert_eq!(
        core.cancel_investigation(&investigation.id)?.status,
        InvestigationStatus::Cancelled
    );
    let reopened = Core::open(temporary.path().join("data"))?;
    assert_eq!(
        reopened.investigations(&project.id, 10)?[0].status,
        InvestigationStatus::Cancelled
    );
    let diagnostics = reopened.diagnostics()?;
    assert_eq!(diagnostics.schema_version, 4);
    assert!(diagnostics.active_runs.is_empty());
    let invalid = Investigation {
        project_id: project.id.clone(),
        target: "Invalid".into(),
        reason: "Invalid".into(),
        budget: InvestigationBudget {
            max_retries: 11,
            ..InvestigationBudget::default()
        },
        ..Investigation::default()
    };
    assert!(core.save_investigation(invalid).is_err());
    Ok(())
}

#[test]
fn known_source_changes_while_queued_block_execution_before_the_runner_starts() -> Result<()> {
    let source = "import test from 'node:test';test('long',async()=>{console.log('runner-started');await new Promise(resolve=>setTimeout(resolve,20000));});";
    let (_temporary, core, project) = fixture(source, true)?;
    fs::write(
        project.path.join("test/other.test.mjs"),
        "import test from 'node:test';test('other',()=>{});",
    )?;
    let report = core.scan(&project)?;
    let first_id = report
        .candidates
        .iter()
        .find(|candidate| candidate.file.ends_with("calc.test.mjs"))
        .context("First candidate")?
        .id
        .clone();
    let second_id = report
        .candidates
        .iter()
        .find(|candidate| candidate.file.ends_with("other.test.mjs"))
        .context("Second candidate")?
        .id
        .clone();
    let options = RunOptions {
        max_concurrency: 1,
        ..RunOptions::default()
    };
    let first = core.start_run(
        core.plan_run(&project.id, &[first_id], options.clone())?,
        true,
    )?;
    let started = Instant::now();
    while !core
        .run_output(&first.id, 0, 65_536)?
        .text
        .contains("runner-started")
    {
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(5),
            "First runner did not start"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let second = core.start_run(core.plan_run(&project.id, &[second_id], options)?, true)?;
    assert_eq!(
        core.run(&second.id)?.context("Queued run")?.status,
        RunStatus::Queued
    );
    fs::write(
        project.path.join("src/calc.mjs"),
        "export const addition=(a,b)=>a-b;\n",
    )?;
    core.cancel_run(&first.id)?;
    let deadline = Instant::now();
    loop {
        let run = core.run(&second.id)?.context("Second run")?;
        if run.status.is_final() {
            assert_eq!(run.status, RunStatus::Blocked, "{run:?}");
            assert!(run.results.is_empty());
            assert!(
                run.message
                    .as_deref()
                    .is_some_and(|message| message.contains("queued"))
            );
            break;
        }
        anyhow::ensure!(
            deadline.elapsed() < Duration::from_secs(5),
            "Queued run did not finalize"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn passive_source_identity_does_not_execute_configured_git_filters() -> Result<()> {
    let (temporary, core, project) = fixture(passing_source(), true)?;
    let marker = temporary.path().join("filter-executed");
    let quoted = format!("'{}'", marker.to_string_lossy().replace('\'', "'\"'\"'"));
    let filter = format!("touch {quoted}; cat");
    git(&project.path, &["config", "filter.canary.clean", &filter])?;
    git(&project.path, &["config", "filter.canary.required", "true"])?;
    fs::write(project.path.join(".gitattributes"), "*.mjs filter=canary\n")?;
    fs::write(
        project.path.join("src/calc.mjs"),
        "export const addition=(a,b)=>a-b;\n",
    )?;
    let source = core.source_state(&project.id)?;
    assert!(source.complete, "{source:?}");
    assert!(
        !marker.exists(),
        "Passive source inspection executed a project-configured Git clean filter"
    );
    Ok(())
}
