use anyhow::{Context, Result};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use verifact_core::{Core, ExecutionPlan, RunOptions, RunRecord, RunStatus};

fn fixture(source: &str, options: RunOptions) -> Result<(TempDir, Core, ExecutionPlan)> {
    let directory = TempDir::new()?;
    let project = directory.path().join("project");
    fs::create_dir_all(project.join("test"))?;
    fs::write(
        project.join("package.json"),
        r#"{"private":true,"scripts":{"test":"node --test"}}"#,
    )?;
    fs::write(project.join("test/example.test.mjs"), source)?;
    let core = Core::open(directory.path().join("data"))?;
    let project = core.add_project(project)?;
    core.scan(&project)?;
    let plan = core.plan_run(&project.id, &[], options)?;
    Ok((directory, core, plan))
}
fn wait(core: &Core, id: &str) -> Result<RunRecord> {
    let started = Instant::now();
    loop {
        let record = core.run(id)?.context("Run not persisted")?;
        if record.status.is_final() {
            return Ok(record);
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(15),
            "Run did not finish: {record:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn wait_file(path: &Path) -> Result<()> {
    let started = Instant::now();
    while !path.exists() {
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(5),
            "Fixture did not create {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn actual_node_assertions_persist_results_output_and_checksums() -> Result<()> {
    let (temporary, core, plan) = fixture(
        "import test from 'node:test'; import assert from 'node:assert/strict'; test('addition',()=>{ console.log('password=private-value'); assert.equal(2+2,4); assert.equal(process.env.NODE_OPTIONS, undefined); });",
        RunOptions::default(),
    )?;
    let project_id = plan.project_id.clone();
    let record = core.start_run(plan, true)?;
    let finished = wait(&core, &record.id)?;
    assert_eq!(finished.status, RunStatus::Passed, "{finished:?}");
    assert!(!finished.results.is_empty());
    assert!(
        finished
            .results
            .iter()
            .any(|result| result.name.contains("addition") && result.status == RunStatus::Passed)
    );
    assert!(
        finished
            .results
            .iter()
            .all(|result| result.candidate_id.is_some())
    );
    assert!(finished.finished_at.is_some());
    assert!(
        finished
            .artifacts
            .iter()
            .all(|artifact| artifact.sha256.is_some())
    );
    let output = core.run_output(&record.id, 0, 65_536)?;
    assert!(!output.text.contains("private-value"));
    for artifact in &finished.artifacts {
        let bytes = fs::read(core.data_dir().join(&artifact.path))?;
        assert!(!String::from_utf8_lossy(&bytes).contains("private-value"));
    }
    let reopened = Core::open(temporary.path().join("data"))?;
    assert_eq!(reopened.run(&record.id)?, Some(finished.clone()));
    assert_eq!(
        reopened.run_history(&project_id, 1)?,
        vec![finished.clone()]
    );
    let after_cancel = reopened.cancel_run(&record.id)?;
    assert_eq!(
        after_cancel, finished,
        "Finalized records cannot be changed by cancellation"
    );
    Ok(())
}

#[test]
fn trust_and_plan_tampering_are_rejected_without_execution() -> Result<()> {
    let (_temporary, core, mut plan) = fixture(
        "import test from 'node:test'; test('safe',()=>{});",
        RunOptions::default(),
    )?;
    assert!(core.start_run(plan.clone(), false).is_err());
    assert!(core.run_history(&plan.project_id, 10)?.is_empty());
    plan.commands[0]
        .args
        .push("--arbitrary-caller-argument".into());
    assert!(core.start_run(plan.clone(), true).is_err());
    assert!(core.run_history(&plan.project_id, 10)?.is_empty());
    assert!(
        core.plan_run(
            &plan.project_id,
            &["foreign-candidate".into()],
            RunOptions::default()
        )
        .is_err()
    );
    assert!(core.run_output("../../outside", 0, 100).is_err());
    Ok(())
}

#[test]
fn actual_assertion_failure_is_not_an_infrastructure_success() -> Result<()> {
    let (_temporary, core, plan) = fixture(
        "import test from 'node:test'; import assert from 'node:assert/strict'; test('regression',()=>assert.equal(2+2,5));",
        RunOptions::default(),
    )?;
    let started = core.start_run(plan, true)?;
    let finished = wait(&core, &started.id)?;
    assert_eq!(finished.status, RunStatus::Failed, "{finished:?}");
    assert!(
        finished
            .results
            .iter()
            .any(|result| result.status == RunStatus::Failed)
    );
    assert_ne!(finished.exit_code, Some(0));
    Ok(())
}

#[test]
fn zero_native_cases_cannot_pass() -> Result<()> {
    let (_temporary, core, plan) = fixture(
        "// Intentionally empty module, no test declarations.\n",
        RunOptions::default(),
    )?;
    let started = core.start_run(plan, true)?;
    let finished = wait(&core, &started.id)?;
    // Node may emit a file-level success for empty modules. Its parser must not
    // elevate a file completion into an asserted individual test.
    assert_ne!(finished.status, RunStatus::Passed, "{finished:?}");
    Ok(())
}

#[test]
fn process_failure_without_assertion_results_is_an_infrastructure_error() -> Result<()> {
    let (_temporary, core, plan) = fixture("process.exit(7);", RunOptions::default())?;
    let started = core.start_run(plan, true)?;
    let finished = wait(&core, &started.id)?;
    assert_eq!(
        finished.status,
        RunStatus::InfrastructureError,
        "{finished:?}"
    );
    assert_ne!(finished.exit_code, Some(0));
    Ok(())
}

#[test]
fn timeout_terminates_a_real_node_process() -> Result<()> {
    let options = RunOptions {
        timeout_ms: 500,
        ..RunOptions::default()
    };
    let (_temporary, core, plan) = fixture(
        "import test from 'node:test'; test('long',async()=>new Promise(resolve=>setTimeout(resolve,20000)));",
        options,
    )?;
    let started = core.start_run(plan, true)?;
    let finished = wait(&core, &started.id)?;
    assert_eq!(finished.status, RunStatus::TimedOut, "{finished:?}");
    assert!(finished.duration_ms.is_some_and(|duration| duration < 5000));
    Ok(())
}

#[test]
fn cancellation_terminates_descendants_and_is_visible_across_core_instances() -> Result<()> {
    let source = r#"import test from 'node:test'; import {spawn} from 'node:child_process';
    test('long',async()=>{ spawn(process.execPath,['-e',`const fs=require('fs'); setInterval(()=>fs.appendFileSync('heartbeat','x'),20);setTimeout(()=>process.exit(),5000)`],{stdio:'ignore'}); await new Promise(resolve=>setTimeout(resolve,20000)); });"#;
    let (_temporary, core, plan) = fixture(source, RunOptions::default())?;
    let heartbeat = plan.project_path.join("heartbeat");
    let started = core.start_run(plan, true)?;
    wait_file(&heartbeat)?;
    let other = Core::open(core.data_dir())?;
    other.cancel_run(&started.id)?;
    let finished = wait(&other, &started.id)?;
    assert_eq!(finished.status, RunStatus::Cancelled, "{finished:?}");
    let size = fs::metadata(&heartbeat)?.len();
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        fs::metadata(&heartbeat)?.len(),
        size,
        "A descendant continued running after cancellation"
    );
    Ok(())
}

#[test]
fn queued_runs_honor_concurrency_and_can_be_cancelled_before_start() -> Result<()> {
    let options = RunOptions {
        max_concurrency: 1,
        ..RunOptions::default()
    };
    let (_temporary, core, first_plan) = fixture(
        "import test from 'node:test'; import fs from 'node:fs'; test('long',async()=>{fs.writeFileSync('started','yes'); await new Promise(resolve=>setTimeout(resolve,20000));});",
        options.clone(),
    )?;
    let project_id = first_plan.project_id.clone();
    let marker = first_plan.project_path.join("started");
    let first = core.start_run(first_plan, true)?;
    wait_file(&marker)?;
    assert!(
        core.start_run(core.plan_run(&project_id, &[], options.clone())?, true)
            .is_err(),
        "Duplicate active candidates must not execute twice"
    );
    let project = core.store()?.project(&project_id)?.context("Project")?;
    fs::write(
        project.path.join("test/other.test.mjs"),
        "import test from 'node:test'; test('other',()=>{});",
    )?;
    let report = core.scan(&project)?;
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.file.ends_with("other.test.mjs"))
        .context("Other candidate")?;
    let second = core.start_run(
        core.plan_run(&project_id, std::slice::from_ref(&candidate.id), options)?,
        true,
    )?;
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        core.run(&second.id)?.context("Second run")?.status,
        RunStatus::Queued
    );
    core.cancel_run(&second.id)?;
    assert_eq!(wait(&core, &second.id)?.status, RunStatus::Cancelled);
    assert_eq!(core.run_output(&second.id, 0, 100)?.next_offset, 0);
    core.cancel_run(&first.id)?;
    assert_eq!(wait(&core, &first.id)?.status, RunStatus::Cancelled);
    Ok(())
}

#[test]
fn output_is_bounded_and_paged() -> Result<()> {
    let options = RunOptions {
        max_output_bytes: 4096,
        ..RunOptions::default()
    };
    let source = "import test from 'node:test'; test('logs',()=>{ for(let i=0;i<2000;i++) console.log('line '+i+' '+ 'x'.repeat(20)); });";
    let (_temporary, core, plan) = fixture(source, options)?;
    let started = core.start_run(plan, true)?;
    let finished = wait(&core, &started.id)?;
    let page = core.run_output(&started.id, 0, 1000)?;
    let next = core.run_output(&started.id, page.next_offset, 1000)?;
    assert!(page.next_offset <= 1000);
    assert_eq!(next.offset, page.next_offset);
    let artifact = finished
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "output")
        .context("Output artifact")?;
    assert!(artifact.bytes <= 4096);
    assert!(finished.output_truncated);
    assert!(core.run_output(&started.id, 4097, 100).is_err());
    Ok(())
}
