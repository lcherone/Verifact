//! Synthetic metadata-only coverage. No real project suite, configuration value or
//! browser state is copied into the fixture or executed by these checks.
use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use verifact_core::{Project, ScanOptions, ScanReport, Store, scan_project};

const PACKAGE: &str = ".vscode/vision-loop";

fn write(root: &Path, relative: &str, contents: &str) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))?;
    fs::write(path, contents)?;
    Ok(())
}

fn fixture() -> Result<TempDir> {
    let root = TempDir::new()?;
    for relative in [
        "package.json",
        "config.frontend.json",
        "suites/checkout.json",
        "suites/login.json",
    ] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/discovery/vision-loop/.vscode/vision-loop")
            .join(relative);
        write(
            root.path(),
            &format!("{PACKAGE}/{relative}"),
            &fs::read_to_string(source)?,
        )?;
    }
    Ok(root)
}

fn project(root: &Path) -> Result<Project> {
    Store::open_memory()?.add_project(root)
}

fn scan(root: &Path) -> Result<ScanReport> {
    scan_project(&project(root)?, &ScanOptions::default())
}

#[test]
fn recognized_vision_loop_has_one_file_candidate_per_suite_not_per_target() -> Result<()> {
    let root = fixture()?;
    let first = scan(root.path())?;
    let second = scan(root.path())?;
    assert_eq!(first.frameworks.len(), 1);
    let framework = &first.frameworks[0];
    assert_eq!(framework.name, "Vision Loop");
    assert_eq!(framework.language, "JavaScript/TypeScript");
    assert_eq!(framework.package_path, Path::new(PACKAGE));
    assert_eq!(framework.suggested_command, None);
    assert_eq!(
        framework.configurations,
        [PathBuf::from(format!("{PACKAGE}/config.frontend.json"))]
    );
    assert_eq!(first.candidates.len(), 2);
    assert_eq!(first.candidates, second.candidates);
    assert!(
        first
            .candidates
            .iter()
            .all(|candidate| candidate.framework_id == framework.id
                && candidate.discovery_kind == "file_candidate")
    );
    assert_eq!(first.files_inspected, 4);
    assert!(first.warnings.is_empty());
    Ok(())
}

#[test]
fn only_explicit_metadata_paths_are_admitted_from_editor_workspace() -> Result<()> {
    let root = fixture()?;
    for relative in [
        ".vscode/credentials.test.js",
        ".vscode/unrelated/package.json",
        ".vscode/vision-loop/storage/auth.test.js",
        ".vscode/vision-loop/actions/private.test.js",
        ".vscode/vision-loop/runs/history.test.js",
        ".vscode/vision-loop/node_modules/private/package.json",
        ".vscode/vision-loop/suites/nested/ignored.json",
    ] {
        write(root.path(), relative, "PRIVATE_SENTINEL not valid JSON")?;
    }
    // Even configuration values are not parsed, only these exact paths recorded.
    write(
        root.path(),
        ".vscode/vision-loop/config.frontend.json",
        "PRIVATE_SENTINEL not valid JSON",
    )?;
    let report = scan(root.path())?;
    assert_eq!(report.files_inspected, 4);
    assert_eq!(report.frameworks.len(), 1);
    assert_eq!(report.candidates.len(), 2);
    assert!(report.warnings.is_empty());
    assert!(!serde_json::to_string(&report)?.contains("PRIVATE_SENTINEL"));
    // Replacing caller exclusions must not accidentally open .vscode broadly.
    let report = scan_project(
        &project(root.path())?,
        &ScanOptions {
            excluded_directories: Vec::new(),
            ..ScanOptions::default()
        },
    )?;
    assert_eq!(report.files_inspected, 4);
    Ok(())
}

#[test]
fn arbitrary_vision_loop_directory_is_not_treated_as_a_known_harness() -> Result<()> {
    let root = fixture()?;
    write(
        root.path(),
        ".vscode/vision-loop/package.json",
        r#"{"name":"unrelated-tool","scripts":{"run":"node not-vision-loop.mjs"},"dependencies":{"@playwright/test":"1"}}"#,
    )?;
    let report = scan(root.path())?;
    assert!(report.frameworks.is_empty());
    assert!(report.candidates.is_empty());
    assert_eq!(report.files_inspected, 0);
    Ok(())
}

#[test]
fn package_name_alone_does_not_identify_a_harness() -> Result<()> {
    let root = fixture()?;
    write(
        root.path(),
        ".vscode/vision-loop/package.json",
        r#"{"name":"example-vision-loop"}"#,
    )?;
    let report = scan(root.path())?;
    assert!(report.frameworks.is_empty());
    assert!(report.candidates.is_empty());
    Ok(())
}

#[test]
fn malformed_and_oversized_suite_metadata_produces_private_bounded_warnings() -> Result<()> {
    let root = fixture()?;
    write(
        root.path(),
        ".vscode/vision-loop/suites/invalid.json",
        r#"{"targets":"PRIVATE_SENTINEL"}"#,
    )?;
    write(
        root.path(),
        ".vscode/vision-loop/suites/oversized.json",
        &" ".repeat(1024 * 1024 + 1),
    )?;
    let report = scan(root.path())?;
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.warnings.len(), 2);
    assert!(!serde_json::to_string(&report)?.contains("PRIVATE_SENTINEL"));
    Ok(())
}

#[test]
fn vision_loop_discovery_obeys_shared_file_budget_and_explicit_exclusion() -> Result<()> {
    let root = fixture()?;
    let options = ScanOptions {
        max_files: 2,
        ..ScanOptions::default()
    };
    let report = scan_project(&project(root.path())?, &options)?;
    assert_eq!(report.files_inspected, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.message.contains("Scan limit reached"))
    );
    let mut options = ScanOptions::default();
    options.excluded_directories.push("vision-loop".into());
    let report = scan_project(&project(root.path())?, &options)?;
    assert_eq!(report.files_inspected, 0);
    assert!(report.frameworks.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn vision_loop_metadata_cannot_follow_symlinks_outside_the_project() -> Result<()> {
    let root = fixture()?;
    let outside = TempDir::new()?;
    write(
        outside.path(),
        "private.json",
        r#"{"targets":[{"secret":"PRIVATE_SENTINEL"}]}"#,
    )?;
    std::os::unix::fs::symlink(
        outside.path().join("private.json"),
        root.path().join(".vscode/vision-loop/suites/private.json"),
    )?;
    let report = scan(root.path())?;
    assert_eq!(report.candidates.len(), 2);
    assert!(!serde_json::to_string(&report)?.contains("PRIVATE_SENTINEL"));
    let escaped = TempDir::new()?;
    std::os::unix::fs::symlink(root.path().join(".vscode"), escaped.path().join(".vscode"))?;
    assert!(scan(escaped.path())?.candidates.is_empty());
    Ok(())
}
