use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use verifact_core::{ScanOptions, ScanReport, Store, scan_project};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/discovery")
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn scan_fixture(name: &str) -> Result<ScanReport> {
    let store = Store::open_memory()?;
    let project = store.add_project(fixtures().join(name))?;
    let report = scan_project(&project, &ScanOptions::default())?;
    assert_eq!(report.project_id, project.id);
    assert!(
        report.files_inspected > 0,
        "{name}: inspect real fixture files"
    );
    for candidate in &report.candidates {
        assert_eq!(candidate.discovery_kind, "file_candidate");
        assert!(!candidate.file.is_absolute());
        assert!(project.path.join(&candidate.file).is_file());
        assert!(
            report
                .frameworks
                .iter()
                .any(|framework| framework.id == candidate.framework_id)
        );
    }
    Ok(report)
}

fn assert_candidate(report: &ScanReport, framework_name: &str, package: &str, file: &str) {
    let package = if package.is_empty() { "." } else { package };
    let framework = report
        .frameworks
        .iter()
        .find(|framework| {
            framework.name == framework_name && framework.package_path == Path::new(package)
        })
        .unwrap_or_else(|| {
            panic!(
                "Missing {framework_name} for package {package:?}: {:?}",
                report.frameworks
            )
        });
    assert!(
        !framework.evidence.is_empty(),
        "Detection must explain its evidence"
    );
    let matches: Vec<_> = report
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.framework_id == framework.id && candidate.file == Path::new(file)
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "Expected one {framework_name} file candidate for {file}"
    );
}

#[test]
fn representative_php_and_node_fixtures_detect_frameworks_and_files() -> Result<()> {
    let cases = [
        ("phpunit-basic", "PHPUnit", "tests/ArithmeticTest.php"),
        ("phpunit-legacy", "PHPUnit", "legacy-tests/LegacyTest.php"),
        ("pest", "Pest", "tests/Feature/CheckoutTest.php"),
        ("vitest", "Vitest", "src/arithmetic.test.ts"),
        ("jest", "Jest", "__tests__/arithmetic.test.cjs"),
        ("playwright", "Playwright", "e2e/checkout.spec.ts"),
        (
            "codeception",
            "Codeception",
            "tests/unit/ArithmeticTest.php",
        ),
        ("cypress", "Cypress", "cypress/e2e/checkout.cy.js"),
        ("mocha", "Mocha", "test/arithmetic.test.js"),
        ("node-test", "Node test", "test/arithmetic.test.mjs"),
    ];
    for (fixture, framework, file) in cases {
        let report = scan_fixture(fixture)?;
        assert_candidate(&report, framework, "", file);
        assert!(
            report.warnings.is_empty(),
            "{fixture}: {:?}",
            report.warnings
        );
    }
    Ok(())
}

#[test]
fn one_file_with_multiple_php_test_methods_remains_one_file_candidate() -> Result<()> {
    let report = scan_fixture("phpunit-basic")?;
    assert_eq!(
        report.candidates.len(),
        1,
        "Passive scanning must not pretend file count is individual test count"
    );
    assert_eq!(report.candidates[0].discovery_kind, "file_candidate");
    Ok(())
}

#[test]
fn mixed_php_node_project_keeps_both_frameworks_at_the_same_root() -> Result<()> {
    let report = scan_fixture("mixed")?;
    assert_candidate(&report, "PHPUnit", "", "tests/PriceTest.php");
    assert_candidate(&report, "Vitest", "", "tests/price.test.js");
    assert_eq!(report.candidates.len(), 2);
    Ok(())
}

#[test]
fn monorepo_candidates_belong_to_their_nearest_package() -> Result<()> {
    let report = scan_fixture("monorepo")?;
    assert_candidate(
        &report,
        "Jest",
        "packages/api",
        "packages/api/src/health.test.js",
    );
    assert_candidate(
        &report,
        "Vitest",
        "packages/web",
        "packages/web/src/cart.test.ts",
    );
    assert_eq!(report.candidates.len(), 2);
    assert!(
        report
            .frameworks
            .iter()
            .all(|framework| framework.package_path != Path::new("."))
    );
    Ok(())
}

#[test]
fn no_tests_is_empty_and_malformed_manifests_are_reported() -> Result<()> {
    let empty = scan_fixture("no-tests")?;
    assert!(empty.candidates.is_empty());
    assert!(empty.frameworks.is_empty());
    assert!(empty.warnings.is_empty());

    let malformed = scan_fixture("malformed")?;
    assert!(malformed.candidates.is_empty());
    for filename in ["package.json", "composer.json"] {
        assert!(
            malformed
                .warnings
                .iter()
                .any(|warning| warning.path == Path::new(filename)),
            "Missing warning for {filename}: {:?}",
            malformed.warnings
        );
    }
    Ok(())
}

#[test]
fn rescans_are_deterministic_and_different_projects_have_distinct_ids() -> Result<()> {
    let temporary = TempDir::new()?;
    let first_path = temporary.path().join("first");
    let second_path = temporary.path().join("second");
    copy_directory(&fixtures().join("vitest"), &first_path)?;
    copy_directory(&fixtures().join("vitest"), &second_path)?;
    let store = Store::open_memory()?;
    let first = store.add_project(&first_path)?;
    assert_eq!(first.id, store.add_project(&first_path)?.id);
    let second = store.add_project(&second_path)?;
    assert_ne!(first.id, second.id);
    let options = ScanOptions::default();
    let before = scan_project(&first, &options)?;
    let after = scan_project(&first, &options)?;
    assert_eq!(before.frameworks, after.frameworks);
    assert_eq!(before.candidates, after.candidates);
    assert_eq!(before.warnings, after.warnings);
    assert_eq!(before.files_inspected, after.files_inspected);
    let other = scan_project(&second, &options)?;
    let ids: HashSet<_> = before
        .candidates
        .iter()
        .map(|candidate| &candidate.id)
        .collect();
    assert!(
        other
            .candidates
            .iter()
            .all(|candidate| !ids.contains(&candidate.id))
    );
    Ok(())
}

#[test]
fn persisted_scan_history_survives_reopening_and_latest_reflects_rescan() -> Result<()> {
    let temporary = TempDir::new()?;
    let project_path = temporary.path().join("project");
    let database = temporary.path().join("verifact.sqlite");
    copy_directory(&fixtures().join("vitest"), &project_path)?;
    let project_id;
    {
        let store = Store::open(&database)?;
        let project = store.add_project(&project_path)?;
        project_id = project.id.clone();
        let initial = scan_project(&project, &ScanOptions::default())?;
        store.save_scan(&initial)?;
        fs::write(
            project_path.join("src/second.test.ts"),
            "import { test } from 'vitest';\ntest('second', () => {});\n",
        )?;
        let refreshed = scan_project(&project, &ScanOptions::default())?;
        assert_eq!(refreshed.candidates.len(), initial.candidates.len() + 1);
        store.save_scan(&refreshed)?;
    }
    let reopened = Store::open(&database)?;
    assert_eq!(reopened.projects()?.len(), 1);
    let latest = reopened
        .latest_scan(&project_id)?
        .context("Saved scan should remain after reopening")?;
    assert_eq!(latest.candidates.len(), 2);
    assert_eq!(reopened.scan_history(&project_id, 10)?.len(), 2);
    assert_eq!(reopened.scan_history(&project_id, 1)?.len(), 1);
    Ok(())
}

#[test]
fn passive_scan_never_runs_manifest_scripts_or_executable_configuration() -> Result<()> {
    let temporary = TempDir::new()?;
    let path = temporary.path();
    fs::create_dir(path.join("tests"))?;
    fs::write(
        path.join("package.json"),
        r#"{"name":"passive-only","scripts":{"test":"node bait.cjs"},"devDependencies":{"vitest":"^3.0.0"}}"#,
    )?;
    fs::write(
        path.join("bait.cjs"),
        "require('node:fs').writeFileSync('script-executed', 'unsafe');\n",
    )?;
    fs::write(
        path.join("vitest.config.cjs"),
        "require('node:fs').writeFileSync('config-executed', 'unsafe'); module.exports = {};\n",
    )?;
    fs::write(
        path.join("tests/passive.test.js"),
        "import { test } from 'vitest'; test('passive', () => {});\n",
    )?;
    let store = Store::open_memory()?;
    let project = store.add_project(path)?;
    let report = scan_project(&project, &ScanOptions::default())?;
    assert_candidate(&report, "Vitest", "", "tests/passive.test.js");
    assert!(!path.join("script-executed").exists());
    assert!(!path.join("config-executed").exists());
    Ok(())
}

#[test]
fn default_and_custom_exclusions_skip_dependency_and_private_test_trees() -> Result<()> {
    let temporary = TempDir::new()?;
    let project_path = temporary.path().join("project");
    copy_directory(&fixtures().join("phpunit-basic"), &project_path)?;
    for directory in [
        "node_modules/foreign/tests",
        "vendor/foreign/tests",
        "private-tests",
    ] {
        fs::create_dir_all(project_path.join(directory))?;
        fs::write(
            project_path.join(directory).join("HiddenTest.php"),
            "<?php // excluded candidate\n",
        )?;
    }
    let store = Store::open_memory()?;
    let project = store.add_project(&project_path)?;
    let mut options = ScanOptions::default();
    options.excluded_directories.push("private-tests".into());
    let report = scan_project(&project, &options)?;
    assert_eq!(report.candidates.len(), 1);
    assert_candidate(&report, "PHPUnit", "", "tests/ArithmeticTest.php");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_the_project_or_introduce_traversal_cycles() -> Result<()> {
    use std::os::unix::fs::symlink;
    let temporary = TempDir::new()?;
    let project_path = temporary.path().join("project");
    let external_path = temporary.path().join("external");
    copy_directory(&fixtures().join("vitest"), &project_path)?;
    fs::create_dir(&external_path)?;
    fs::write(
        external_path.join("leaked.test.ts"),
        "import { test } from 'vitest'; test('outside', () => {});\n",
    )?;
    symlink(&external_path, project_path.join("external-link"))?;
    symlink(
        external_path.join("leaked.test.ts"),
        project_path.join("src/leaked.test.ts"),
    )?;
    symlink(&project_path, project_path.join("cycle"))?;
    let store = Store::open_memory()?;
    let project = store.add_project(project_path)?;
    let report = scan_project(&project, &ScanOptions::default())?;
    assert_eq!(report.candidates.len(), 1);
    assert_candidate(&report, "Vitest", "", "src/arithmetic.test.ts");
    Ok(())
}

#[test]
fn a_scan_file_budget_reports_incompleteness() -> Result<()> {
    let store = Store::open_memory()?;
    let project = store.add_project(fixtures().join("mixed"))?;
    let options = ScanOptions {
        max_files: 1,
        ..ScanOptions::default()
    };
    let report = scan_project(&project, &options)?;
    assert!(report.files_inspected <= 1);
    assert!(
        !report.warnings.is_empty(),
        "A truncated scan must not look complete"
    );
    Ok(())
}
