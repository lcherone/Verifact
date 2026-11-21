//! Opt-in real framework validation. Install fixtures/execution into an isolated
//! external fixture directory; never point this at a user project.
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use verifact_core::{
    Framework, Project, RunStatus, TestCandidate,
    adapters::{parse_results, plan_command},
    stable_id,
};

fn records(root: &Path, name: &str, file: &str) -> (Project, Framework, TestCandidate) {
    let project = Project {
        id: "synthetic-adapter-fixture".into(),
        name: "Generic execution fixture".into(),
        path: root.into(),
        created_at: "fixture".into(),
    };
    let framework = Framework {
        id: stable_id(&[&project.id, ".", name]),
        package_path: PathBuf::from("."),
        name: name.into(),
        language: "fixture".into(),
        evidence: vec![],
        configurations: if name == "Playwright" {
            vec!["playwright.config.cjs".into()]
        } else if name == "Jest" {
            vec!["jest.config.cjs".into()]
        } else {
            vec![]
        },
        suggested_command: None,
    };
    let candidate = TestCandidate {
        id: stable_id(&[&project.id, &framework.id, file]),
        framework_id: framework.id.clone(),
        file: file.into(),
        display_name: file.into(),
        discovery_kind: "file_candidate".into(),
    };
    (project, framework, candidate)
}

#[test]
#[ignore = "requires isolated installed synthetic frameworks; VERIFACT_ADAPTER_FIXTURE_ROOT"]
fn real_frameworks_report_assertion_pass_skip_and_failure() {
    let fixture = PathBuf::from(
        std::env::var_os("VERIFACT_ADAPTER_FIXTURE_ROOT").expect("fixture root required"),
    )
    .canonicalize()
    .unwrap();
    for (name, package, pass, fail) in [
        (
            "Node test",
            "javascript",
            "test/node.pass.test.mjs",
            "test/node.fail.test.mjs",
        ),
        (
            "Jest",
            "javascript",
            "test/jest.pass.test.cjs",
            "test/jest.fail.test.cjs",
        ),
        (
            "Vitest",
            "javascript",
            "test/vitest.pass.test.mjs",
            "test/vitest.fail.test.mjs",
        ),
        (
            "Mocha",
            "javascript",
            "test/mocha.pass.test.cjs",
            "test/mocha.fail.test.cjs",
        ),
        (
            "Playwright",
            "javascript",
            "test/playwright.pass.spec.cjs",
            "test/playwright.fail.spec.cjs",
        ),
        (
            "PHPUnit",
            "phpunit",
            "tests/PassingTest.php",
            "tests/FailingTest.php",
        ),
    ] {
        for (file, should_pass) in [(pass, true), (fail, false)] {
            let (project, framework, candidate) = records(&fixture.join(package), name, file);
            let artifacts = tempfile::tempdir().unwrap();
            let command = plan_command(
                &project,
                &framework,
                &[candidate],
                &artifacts.path().canonicalize().unwrap(),
            )
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
            let mut process = Command::new(&command.program);
            process.env_clear();
            for key in [
                "PATH",
                "HOME",
                "TMPDIR",
                "TMP",
                "TEMP",
                "LANG",
                "LC_ALL",
                "SYSTEMROOT",
                "WINDIR",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    process.env(key, value);
                }
            }
            let output = process
                .args(&command.args)
                .current_dir(&command.cwd)
                .env("CI", "1")
                .env("NO_COLOR", "1")
                .envs(&command.environment)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let results = parse_results(&command, &stdout, &stderr, output.status.code())
                .unwrap_or_else(|error| panic!("{name} {file}: {error:#}\n{stdout}\n{stderr}"));
            if should_pass {
                assert!(output.status.success(), "{name}: {stderr}");
                assert!(
                    results
                        .iter()
                        .any(|native| native.status == RunStatus::Passed),
                    "{name}: {results:?}"
                );
                assert!(
                    results
                        .iter()
                        .any(|native| native.status == RunStatus::Skipped),
                    "{name}: {results:?}"
                );
                assert!(
                    results.iter().all(|native| matches!(
                        native.status,
                        RunStatus::Passed | RunStatus::Skipped
                    )),
                    "{name}: {results:?}"
                );
            } else {
                assert!(!output.status.success(), "{name}");
                assert!(
                    results
                        .iter()
                        .any(|native| native.status == RunStatus::Failed),
                    "{name}: {results:?}"
                );
            }
            eprintln!(
                "validated {name}: {} ({:?})",
                if should_pass {
                    "pass + skip"
                } else {
                    "assertion failure"
                },
                command.runtime_versions
            );
        }
    }
}
