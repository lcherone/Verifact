use anyhow::{Context, Result};
use std::{fs, path::Path};
use tempfile::TempDir;
use verifact_core::Core;

fn write_node_project(path: &Path) -> Result<()> {
    fs::create_dir_all(path.join("test"))?;
    fs::write(
        path.join("package.json"),
        r#"{"name":"service-fixture","private":true,"scripts":{"test":"node --test"}}"#,
    )?;
    fs::write(
        path.join("test/addition.test.mjs"),
        "import test from 'node:test'; import assert from 'node:assert/strict'; test('addition', () => assert.equal(2 + 2, 4));\n",
    )?;
    Ok(())
}

#[test]
fn core_persists_the_shared_project_catalogue_scan_and_completed_job() -> Result<()> {
    let temporary = TempDir::new()?;
    let data_dir = temporary.path().join("data");
    let project_dir = temporary.path().join("project");
    write_node_project(&project_dir)?;
    let core = Core::open(&data_dir)?;
    let project = core.add_project(&project_dir)?;
    let report = core.scan(&project)?;
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].discovery_kind, "file_candidate");
    let jobs = core.store()?.jobs(&project.id, 10)?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "completed");
    assert!(jobs[0].finished_at.is_some());
    drop(core);

    let reopened = Core::open(&data_dir)?;
    let store = reopened.store()?;
    assert_eq!(store.projects()?, vec![project.clone()]);
    assert_eq!(store.latest_scan(&project.id)?, Some(report));
    assert_eq!(store.jobs(&project.id, 10)?, jobs);
    Ok(())
}

#[test]
fn failed_rescan_is_finalized_and_does_not_replace_previous_catalogue() -> Result<()> {
    let temporary = TempDir::new()?;
    let project_dir = temporary.path().join("project");
    write_node_project(&project_dir)?;
    let core = Core::open(temporary.path().join("data"))?;
    let project = core.add_project(&project_dir)?;
    let report = core.scan(&project)?;
    fs::remove_dir_all(&project_dir)?;

    assert!(
        core.scan(&project).is_err(),
        "A missing source directory cannot be a successful scan"
    );
    let store = core.store()?;
    assert_eq!(store.latest_scan(&project.id)?, Some(report));
    assert_eq!(store.scan_history(&project.id, 10)?.len(), 1);
    let jobs = store.jobs(&project.id, 10)?;
    assert_eq!(jobs.len(), 2);
    let failed = jobs
        .iter()
        .find(|job| job.status == "failed")
        .context("The failed scan must leave a finalized failed job")?;
    assert!(failed.finished_at.is_some());
    assert!(!failed.message.is_empty());
    assert!(jobs.iter().all(|job| job.status != "running"));
    Ok(())
}

#[test]
fn malformed_metadata_completes_with_visible_warnings_and_no_invented_candidates() -> Result<()> {
    let temporary = TempDir::new()?;
    let project_dir = temporary.path().join("project");
    fs::create_dir(&project_dir)?;
    fs::write(project_dir.join("package.json"), "{ broken JSON")?;
    fs::write(project_dir.join("composer.json"), "{ also broken JSON")?;
    let core = Core::open(temporary.path().join("data"))?;
    let project = core.add_project(project_dir)?;
    let report = core.scan(&project)?;
    assert!(report.candidates.is_empty());
    assert!(report.frameworks.is_empty());
    assert_eq!(report.warnings.len(), 2);
    let store = core.store()?;
    assert_eq!(store.latest_scan(&project.id)?, Some(report));
    let jobs = store.jobs(&project.id, 10)?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "completed");
    assert!(jobs[0].finished_at.is_some());
    Ok(())
}

#[test]
fn rejected_project_directory_does_not_create_registry_entries() -> Result<()> {
    let temporary = TempDir::new()?;
    let core = Core::open(temporary.path().join("data"))?;
    let file = temporary.path().join("not-a-directory");
    fs::write(&file, "source file")?;
    assert!(core.add_project(file).is_err());
    assert!(core.add_project(temporary.path().join("missing")).is_err());
    assert!(core.store()?.projects()?.is_empty());
    Ok(())
}

#[test]
fn project_configuration_survives_reopen_readd_and_rescan() -> Result<()> {
    let temporary = TempDir::new()?;
    let project_dir = temporary.path().join("project");
    write_node_project(&project_dir)?;
    let data_dir = temporary.path().join("data");
    let core = Core::open(&data_dir)?;
    let original = core.add_project(&project_dir)?;
    assert_eq!(core.project_base_url(&original.id)?, None);
    let configured = core.configure_project(
        &original.id,
        Some("  Named project  "),
        Some(" https://example.test "),
    )?;
    assert_eq!(configured.name, "Named project");
    assert_eq!(configured.id, original.id);
    assert_eq!(configured.path, original.path);
    assert_eq!(configured.created_at, original.created_at);
    assert_eq!(core.add_project(&project_dir)?, configured);
    core.scan(&original)?;
    assert_eq!(
        core.store()?.project(&original.id)?,
        Some(configured.clone())
    );
    assert_eq!(core.store()?.scan_history(&original.id, 10)?.len(), 1);
    drop(core);

    let reopened = Core::open(data_dir)?;
    assert_eq!(
        reopened.store()?.project(&original.id)?,
        Some(configured.clone())
    );
    assert_eq!(
        reopened.project_base_url(&original.id)?.as_deref(),
        Some("https://example.test/")
    );
    reopened.configure_project(&original.id, Some("Renamed again"), None)?;
    assert_eq!(
        reopened.project_base_url(&original.id)?.as_deref(),
        Some("https://example.test/")
    );
    let url_only = reopened.configure_project(
        &original.id,
        None,
        Some("http://localhost:4179/path?mode=test"),
    )?;
    assert_eq!(url_only.name, "Renamed again");
    assert_eq!(
        reopened.project_base_url(&original.id)?.as_deref(),
        Some("http://localhost:4179/path?mode=test")
    );
    let other_dir = temporary.path().join("other");
    fs::create_dir(&other_dir)?;
    let other = reopened.add_project(other_dir)?;
    assert_eq!(reopened.project_base_url(&other.id)?, None);
    Ok(())
}

#[test]
fn invalid_project_configuration_preserves_previous_values() -> Result<()> {
    let temporary = TempDir::new()?;
    let core = Core::open(temporary.path().join("data"))?;
    let project = core.add_project(temporary.path())?;
    let configured = core.configure_project(
        &project.id,
        Some("Original name"),
        Some("https://example.test/"),
    )?;
    for url in [
        "",
        "relative/path",
        "ftp://example.test",
        "https://",
        "https:///example.test",
        "http://user:password@example.test",
        "https://user@example.test",
        "https://@example.test",
        "https://exam\nple.test",
        "https://[invalid",
    ] {
        assert!(
            core.configure_project(&project.id, Some("Changed"), Some(url))
                .is_err(),
            "Accepted invalid URL {url:?}"
        );
        assert_eq!(
            core.store()?.project(&project.id)?,
            Some(configured.clone())
        );
        assert_eq!(
            core.project_base_url(&project.id)?.as_deref(),
            Some("https://example.test/")
        );
    }
    assert!(
        core.configure_project(&project.id, Some(" \t "), Some("http://localhost/"))
            .is_err()
    );
    assert_eq!(core.store()?.project(&project.id)?, Some(configured));
    assert!(
        core.configure_project("missing", Some("Name"), Some("http://localhost/"))
            .is_err()
    );
    assert!(core.project_base_url("missing").is_err());
    assert_eq!(core.store()?.projects()?.len(), 1);
    Ok(())
}
