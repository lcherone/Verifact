//! Framework-native command planning and bounded structured result parsing.
//! Planning never starts a project runner or evaluates project configuration.
use crate::{
    AdapterCommand, Framework, NativeTestResult, Project, ReporterFormat, RunStatus, TestCandidate,
    stable_id,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_RESULTS: usize = 100_000;

/// Construct an argv command for approved execution. The caller must establish
/// that framework and candidate records come from the current stored catalogue.
pub fn plan_command(
    project: &Project,
    framework: &Framework,
    candidates: &[TestCandidate],
    artifact_dir: &Path,
) -> Result<AdapterCommand> {
    ensure!(
        !candidates.is_empty(),
        "Blocked: select at least one candidate file"
    );
    let root = project
        .path
        .canonicalize()
        .context("Blocked: project directory is unavailable")?;
    ensure!(root.is_dir(), "Blocked: project path is not a directory");
    let cwd = contained_path(&root, &framework.package_path, false)?;
    let expected = stable_id(&[
        &project.id,
        &framework.package_path.to_string_lossy(),
        &framework.name,
    ]);
    ensure!(
        framework.id == expected,
        "Blocked: framework identity does not belong to this project/package"
    );
    let mut selected = BTreeSet::new();
    let mut files = Vec::new();
    for candidate in candidates {
        ensure!(
            candidate.framework_id == framework.id,
            "Blocked: candidate belongs to another framework"
        );
        ensure!(
            candidate.id
                == stable_id(&[
                    &project.id,
                    &framework.id,
                    &candidate.file.to_string_lossy()
                ]),
            "Blocked: candidate identity does not match its source file"
        );
        ensure!(
            selected.insert(candidate.id.clone()),
            "Blocked: duplicate candidate selection"
        );
        let file = contained_path(&root, &candidate.file, true)?;
        ensure!(
            file.starts_with(&cwd),
            "Blocked: candidate is outside the framework package"
        );
        files.push(file);
    }
    ensure!(
        artifact_dir.is_absolute() && safe_components(artifact_dir, true),
        "Blocked: report directory must be an absolute managed path without parent traversal"
    );
    ensure!(
        !artifact_dir.starts_with(&root),
        "Blocked: runner reports must be outside the selected source project"
    );
    reject_existing_symlinks(artifact_dir)?;
    let mut command = AdapterCommand {
        program: PathBuf::new(),
        args: Vec::new(),
        cwd: cwd.clone(),
        report_path: artifact_dir.join("report.json"),
        format: ReporterFormat::JestJson,
        framework: framework.name.clone(),
        candidate_ids: candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect(),
        runtime_versions: BTreeMap::new(),
        environment: BTreeMap::new(),
    };
    match framework.name.as_str() {
        "Node test" => {
            let runtime = host_runtime("node", &root)?;
            let version = runtime_version(&runtime, false)?;
            ensure!(
                version_major(&version)? >= 22,
                "Blocked: Node test adapter requires Node.js 22 or newer"
            );
            command.program = runtime;
            command.runtime_versions.insert("node".into(), version);
            command.format = ReporterFormat::NodeTap;
            command.report_path = artifact_dir.join("report.tap");
            command.args = vec![
                "--test".into(),
                "--test-concurrency=1".into(),
                "--test-reporter=tap".into(),
                format!(
                    "--test-reporter-destination={}",
                    utf8_path(&command.report_path)?
                ),
                "--test-reporter=spec".into(),
                "--test-reporter-destination=stdout".into(),
            ];
        }
        "Jest" | "Vitest" | "Playwright" | "Mocha" => {
            let (package_name, entry, versions) = match framework.name.as_str() {
                "Jest" => ("jest", "bin/jest.js", 29..=30),
                "Vitest" => ("vitest", "vitest.mjs", 3..=4),
                "Playwright" => ("@playwright/test", "cli.js", 1..=1),
                _ => ("mocha", "bin/mocha.js", 10..=11),
            };
            let (package, version) = installed_node_package(&root, &cwd, package_name)?;
            ensure!(
                versions.contains(&version_major(&version)?),
                "Blocked: {} version {} is outside the validated adapter range",
                framework.name,
                version
            );
            let entry = package
                .join(entry)
                .canonicalize()
                .context("Blocked: installed runner entry point is missing")?;
            ensure!(
                entry.is_file() && entry.starts_with(&root),
                "Blocked: runner entry point escapes the project"
            );
            let runtime = host_runtime("node", &root)?;
            let node_version = runtime_version(&runtime, false)?;
            ensure!(
                version_major(&node_version)? >= 22,
                "Blocked: JavaScript adapters require Node.js 22 or newer"
            );
            command.runtime_versions.insert("node".into(), node_version);
            command
                .runtime_versions
                .insert(framework.name.clone(), version);
            command.program = runtime;
            command.args.push(utf8_path(&entry)?);
            match framework.name.as_str() {
                "Jest" => {
                    command.format = ReporterFormat::JestJson;
                    command.args.extend([
                        "--runInBand".into(),
                        "--watch=false".into(),
                        "--json".into(),
                        format!("--outputFile={}", utf8_path(&command.report_path)?),
                    ]);
                    append_config(
                        &root,
                        framework,
                        "jest.config.",
                        "--config",
                        &mut command.args,
                    )?;
                    command.args.push("--runTestsByPath".into());
                }
                "Vitest" => {
                    reject_vitest_ambiguous_filters(&root, &cwd, &files)?;
                    command.format = ReporterFormat::VitestJson;
                    command.args.extend([
                        "run".into(),
                        "--reporter=json".into(),
                        format!("--outputFile={}", utf8_path(&command.report_path)?),
                        "--maxWorkers=1".into(),
                        "--no-file-parallelism".into(),
                    ]);
                    append_config(
                        &root,
                        framework,
                        "vitest.config.",
                        "--config",
                        &mut command.args,
                    )?;
                }
                "Playwright" => {
                    command.format = ReporterFormat::PlaywrightJson;
                    command.args.extend([
                        "test".into(),
                        "--reporter=json".into(),
                        "--workers=1".into(),
                    ]);
                    append_config(
                        &root,
                        framework,
                        "playwright.config.",
                        "--config",
                        &mut command.args,
                    )?;
                    command.environment.insert(
                        "PLAYWRIGHT_JSON_OUTPUT_FILE".into(),
                        utf8_path(&command.report_path)?,
                    );
                }
                _ => {
                    command.format = ReporterFormat::MochaJson;
                    command.args.extend([
                        "--reporter=json".into(),
                        "--reporter-option".into(),
                        format!("output={}", utf8_path(&command.report_path)?),
                    ]);
                }
            }
        }
        "PHPUnit" => {
            ensure!(
                files.len() == 1,
                "Blocked: PHPUnit commands select one file at a time"
            );
            let vendor = find_installed_directory(&root, &cwd, "vendor/phpunit/phpunit")?;
            let installed = vendor
                .parent()
                .and_then(Path::parent)
                .context("Blocked: invalid PHPUnit vendor directory")?
                .join("composer/installed.json");
            let metadata = read_json_file(&installed, MAX_METADATA_BYTES)?;
            let packages = metadata
                .as_array()
                .or_else(|| metadata.get("packages").and_then(Value::as_array))
                .context("Blocked: Composer installed metadata has no package list")?;
            let version = packages
                .iter()
                .find(|package| {
                    package.get("name").and_then(Value::as_str) == Some("phpunit/phpunit")
                })
                .and_then(|package| package.get("version"))
                .and_then(Value::as_str)
                .context("Blocked: installed PHPUnit version is unavailable")?;
            ensure!(
                (9..=12).contains(&version_major(version)?),
                "Blocked: PHPUnit version {version} is outside the validated adapter range"
            );
            let runner = vendor
                .join("phpunit")
                .canonicalize()
                .context("Blocked: installed PHPUnit executable is missing")?;
            ensure!(
                runner.is_file() && runner.starts_with(&root),
                "Blocked: PHPUnit runner escapes the project"
            );
            let runtime = host_runtime("php", &root)?;
            let php_version = runtime_version(&runtime, true)?;
            let mut parts = php_version.split('.');
            let installed_php = (
                parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0),
                parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0),
            );
            let minimum_php = match version_major(version)? {
                9 => (7, 3),
                10 => (8, 1),
                11 => (8, 2),
                _ => (8, 3),
            };
            ensure!(
                installed_php >= minimum_php,
                "Blocked: installed PHP {php_version} is incompatible with PHPUnit {version}"
            );
            command.runtime_versions.insert("php".into(), php_version);
            command
                .runtime_versions
                .insert("PHPUnit".into(), version.to_owned());
            command.program = runtime;
            command.format = ReporterFormat::PhpunitJunit;
            command.report_path = artifact_dir.join("report.xml");
            command.args = vec![
                utf8_path(&runner)?,
                "--colors=never".into(),
                "--log-junit".into(),
                utf8_path(&command.report_path)?,
            ];
            let configs: Vec<_> = framework
                .configurations
                .iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("phpunit") && name.contains(".xml"))
                })
                .collect();
            ensure!(
                configs.len() <= 1,
                "Blocked: multiple PHPUnit configurations require an explicit execution profile"
            );
            if let Some(config) = configs.first() {
                command.args.extend([
                    "--configuration".into(),
                    utf8_path(&contained_path(&root, config, true)?)?,
                ]);
            }
        }
        "Cypress" => bail!(
            "Blocked: Cypress execution requires a verified installed browser binary/profile; discovery alone does not establish that prerequisite"
        ),
        _ => bail!(
            "Blocked: {} does not yet have an execution adapter; no project script was run",
            framework.name
        ),
    }
    for file in files {
        let path = utf8_path(&file)?;
        command
            .args
            .push(if command.format == ReporterFormat::PlaywrightJson {
                regex_literal(&path.replace('\\', "/"))
            } else {
                path
            });
    }
    Ok(command)
}

// Vitest positional filters are case-insensitive substring filters, not exact
// filenames. Refuse ambiguous selections instead of silently running siblings.
fn reject_vitest_ambiguous_filters(root: &Path, cwd: &Path, selected: &[PathBuf]) -> Result<()> {
    let filters: Vec<_> = selected
        .iter()
        .map(|path| {
            path.strip_prefix(cwd)
                .map(|relative| relative.to_string_lossy().replace('\\', "/").to_lowercase())
        })
        .collect::<std::result::Result<_, _>>()?;
    let mut pending = vec![root.to_path_buf()];
    let mut inspected = 0_usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .context("Blocked: cannot verify exact Vitest file selection")?
        {
            let entry = entry.context("Blocked: cannot inspect Vitest source paths")?;
            inspected += 1;
            ensure!(
                inspected <= 100_000,
                "Blocked: exact Vitest file selection exceeds the 100000-entry verification budget"
            );
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !["node_modules", ".git", ".tools"]
                    .iter()
                    .any(|name| entry.file_name() == *name)
                {
                    pending.push(path);
                }
                continue;
            }
            // Dependency symlinks are inside the excluded directories. Source
            // symlinks can change glob selection and require an explicit profile.
            ensure!(
                !kind.is_symlink(),
                "Blocked: source symlink prevents exact Vitest file selection"
            );
            if !kind.is_file() || selected.contains(&path) {
                continue;
            }
            let relative = path
                .strip_prefix(cwd)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase();
            ensure!(
                !filters.iter().any(|filter| relative.contains(filter)),
                "Blocked: Vitest file filter also matches {}; choose an unambiguous execution profile",
                path.display()
            );
        }
    }
    Ok(())
}

fn safe_components(path: &Path, absolute: bool) -> bool {
    path.components().all(|component| {
        matches!(component, Component::Normal(_) | Component::CurDir)
            || absolute && matches!(component, Component::RootDir | Component::Prefix(_))
    })
}

fn contained_path(root: &Path, relative: &Path, file: bool) -> Result<PathBuf> {
    ensure!(
        !relative.is_absolute() && safe_components(relative, false),
        "Blocked: project-relative path contains traversal or an absolute prefix"
    );
    let path = root
        .join(relative)
        .canonicalize()
        .context("Blocked: selected project source is unavailable")?;
    ensure!(
        path.starts_with(root),
        "Blocked: project source resolves outside the project"
    );
    ensure!(
        if file { path.is_file() } else { path.is_dir() },
        "Blocked: selected project path has the wrong file type"
    );
    Ok(path)
}

fn reject_existing_symlinks(path: &Path) -> Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Blocked: managed report path contains a symbolic link"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).context("Blocked: cannot inspect managed report directory");
            }
        }
    }
    Ok(())
}

fn utf8_path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("Blocked: runner paths must be valid UTF-8")
}

fn find_installed_directory(root: &Path, cwd: &Path, suffix: &str) -> Result<PathBuf> {
    let mut directory = cwd;
    loop {
        let path = directory.join(suffix);
        if path.is_dir() {
            let path = path.canonicalize()?;
            ensure!(
                path.starts_with(root),
                "Blocked: installed runner directory escapes the project"
            );
            return Ok(path);
        }
        if directory == root {
            break;
        }
        directory = directory
            .parent()
            .context("Blocked: cannot resolve runner package")?;
    }
    bail!("Blocked: {suffix} is not installed in this project; install its dependencies explicitly")
}

fn installed_node_package(root: &Path, cwd: &Path, name: &str) -> Result<(PathBuf, String)> {
    let directory = find_installed_directory(root, cwd, &format!("node_modules/{name}"))?;
    let metadata = read_json_file(&directory.join("package.json"), MAX_METADATA_BYTES)?;
    ensure!(
        metadata.get("name").and_then(Value::as_str) == Some(name),
        "Blocked: installed package identity does not match {name}"
    );
    let version = metadata
        .get("version")
        .and_then(Value::as_str)
        .context("Blocked: installed runner version is unavailable")?;
    Ok((directory, version.to_owned()))
}

fn version_major(version: &str) -> Result<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .context("Blocked: runtime/runner version is not a supported release version")
}

fn host_runtime(name: &str, project_root: &Path) -> Result<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if !directory.is_absolute() {
            continue;
        }
        let path = directory.join(name);
        if !path.is_file() {
            continue;
        }
        let path = path.canonicalize()?;
        if path.starts_with(project_root) {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.metadata()?.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return Ok(path);
    }
    bail!("Blocked: host runtime {name} was not found outside the selected project")
}

fn runtime_version(program: &Path, php: bool) -> Result<String> {
    let mut command = Command::new(program);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if php {
        command.arg("-n");
    }
    let mut child = command
        .arg("--version")
        .spawn()
        .context("Blocked: host runtime could not start for its version probe")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Blocked: host runtime version probe timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    ensure!(
        status.success(),
        "Blocked: host runtime version probe failed"
    );
    let mut output = String::new();
    if let Some(stdout) = child.stdout.take() {
        stdout.take(8193).read_to_string(&mut output)?;
    }
    ensure!(
        output.len() <= 8192,
        "Blocked: host runtime returned excessive version output"
    );
    let line = output.lines().next().unwrap_or_default().trim();
    if php {
        let version = line
            .strip_prefix("PHP ")
            .and_then(|line| line.split_whitespace().next())
            .context("Blocked: unrecognized PHP version output")?;
        Ok(version.to_owned())
    } else {
        version_major(line)?;
        Ok(line.to_owned())
    }
}

fn append_config(
    root: &Path,
    framework: &Framework,
    prefix: &str,
    flag: &str,
    args: &mut Vec<String>,
) -> Result<()> {
    let configs: Vec<_> = framework
        .configurations
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        })
        .collect();
    ensure!(
        configs.len() <= 1,
        "Blocked: multiple {} configurations require an explicit execution profile",
        framework.name
    );
    if let Some(config) = configs.first() {
        args.extend([
            flag.into(),
            utf8_path(&contained_path(root, config, true)?)?,
        ]);
    }
    Ok(())
}

fn regex_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if "\\.^$|?*+()[]{}".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Read a native reporter artifact, never treating an empty/missing/malformed
/// report as proof of success. Raw evidence is retained separately by execution.
pub fn parse_results(
    command: &AdapterCommand,
    stdout: &str,
    _stderr: &str,
    exit_code: Option<i32>,
) -> Result<Vec<NativeTestResult>> {
    let text = if command.report_path.exists() {
        read_bounded(&command.report_path, MAX_REPORT_BYTES)?
    } else if command.format == ReporterFormat::NodeTap && !stdout.is_empty() {
        ensure!(
            stdout.len() as u64 <= MAX_REPORT_BYTES,
            "Native TAP output exceeds the parser limit"
        );
        stdout.to_owned()
    } else {
        bail!("Native report is missing; runner output is not verification evidence")
    };
    let mut results = match command.format {
        ReporterFormat::NodeTap => parse_tap(&text)?,
        ReporterFormat::JestJson | ReporterFormat::VitestJson => parse_jest(&text)?,
        ReporterFormat::PhpunitJunit | ReporterFormat::CypressJunit => parse_junit(&text)?,
        ReporterFormat::PlaywrightJson => parse_playwright(&text)?,
        ReporterFormat::MochaJson => parse_mocha(&text)?,
    };
    if command.format == ReporterFormat::NodeTap {
        // Node emits an automatically successful file-level subtest when a
        // module declares no tests. It is process/load evidence, not a test.
        results.retain_mut(|native| {
            let module_level = command.args.iter().any(|argument| {
                let path = Path::new(argument);
                path.is_absolute()
                    && (native.name == *argument
                        || path
                            .strip_prefix(&command.cwd)
                            .is_ok_and(|relative| native.name == relative.to_string_lossy()))
            });
            if !module_level {
                return true;
            }
            if native.status == RunStatus::Passed {
                return false;
            }
            native.status = RunStatus::InfrastructureError;
            native.message = Some(format!(
                "Selected module failed before reporting executable test cases\n{}",
                native.message.as_deref().unwrap_or_default()
            ));
            true
        });
    }
    ensure!(
        results.len() <= MAX_RESULTS,
        "Native report contains too many results"
    );
    if results.is_empty() {
        results.push(result(
            "no-tests",
            "No executable tests reported",
            RunStatus::Blocked,
            None,
            None,
            Some("Runner reported zero executable tests; verification is incomplete".into()),
        ));
    }
    if exit_code != Some(0)
        && results
            .iter()
            .all(|result| matches!(result.status, RunStatus::Passed | RunStatus::Skipped))
    {
        results.push(result(
            "runner-exit",
            "Runner exited without complete successful evidence",
            RunStatus::InfrastructureError,
            None,
            None,
            Some(format!(
                "Process exit {:?} is inconsistent with the available native test results",
                exit_code
            )),
        ));
    }
    for (index, native) in results.iter_mut().enumerate() {
        native.native_id = stable_id(&[&command.framework, &native.native_id, &index.to_string()]);
        if command.candidate_ids.len() == 1 {
            native.candidate_id = command.candidate_ids.first().cloned();
        }
    }
    Ok(results)
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<String> {
    ensure!(
        path.is_absolute() && safe_components(path, true),
        "Report/metadata path is not an absolute contained path"
    );
    reject_existing_symlinks(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .context("Cannot open native report/metadata")?;
    ensure!(
        file.metadata()?.is_file(),
        "Native report/metadata is not a regular file"
    );
    ensure!(
        file.metadata()?.len() <= max_bytes,
        "Native report/metadata exceeds the bounded parser limit"
    );
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max_bytes,
        "Native report/metadata grew beyond the parser limit"
    );
    String::from_utf8(bytes).context("Native reporter output is not valid UTF-8")
}

fn read_json_file(path: &Path, max_bytes: u64) -> Result<Value> {
    serde_json::from_str(&read_bounded(path, max_bytes)?)
        .context("Malformed installed runner metadata")
}

fn result(
    id: &str,
    name: &str,
    status: RunStatus,
    duration_ms: Option<u64>,
    assertions: Option<u64>,
    message: Option<String>,
) -> NativeTestResult {
    NativeTestResult {
        native_id: id.into(),
        candidate_id: None,
        name: name.into(),
        status,
        duration_ms,
        assertions,
        message: message.map(|message| message.chars().take(16_384).collect()),
    }
}

fn number_ms(value: Option<f64>, factor: f64) -> Option<u64> {
    value
        .filter(|value| value.is_finite() && *value >= 0.)
        .map(|value| (value * factor).round() as u64)
}

fn native_status(value: &str) -> RunStatus {
    match value {
        "passed" | "pass" => RunStatus::Passed,
        "failed" | "fail" => RunStatus::Failed,
        "pending" | "skipped" | "todo" | "disabled" => RunStatus::Skipped,
        "timedOut" => RunStatus::TimedOut,
        "interrupted" => RunStatus::Cancelled,
        _ => RunStatus::InfrastructureError,
    }
}

fn parse_jest(text: &str) -> Result<Vec<NativeTestResult>> {
    let report: Value = serde_json::from_str(text).context("Malformed Jest/Vitest JSON report")?;
    let suites = report
        .get("testResults")
        .and_then(Value::as_array)
        .context("Jest/Vitest report has no testResults array")?;
    let mut results = Vec::new();
    for suite in suites {
        let suite_name = suite.get("name").and_then(Value::as_str).unwrap_or("suite");
        let cases = suite.get("assertionResults").and_then(Value::as_array);
        if let Some(cases) = cases {
            for case in cases {
                let name = case
                    .get("fullName")
                    .or_else(|| case.get("title"))
                    .and_then(Value::as_str)
                    .context("Native assertion result has no test name")?;
                let status = case
                    .get("status")
                    .and_then(Value::as_str)
                    .context("Native assertion result has no terminal status")?;
                let messages = case
                    .get("failureMessages")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .filter(|message| !message.is_empty());
                results.push(result(
                    &format!("{suite_name}:{name}"),
                    name,
                    native_status(status),
                    number_ms(case.get("duration").and_then(Value::as_f64), 1.),
                    case.get("numPassingAsserts").and_then(Value::as_u64),
                    messages,
                ));
            }
        }
        if cases.is_none_or(Vec::is_empty)
            && (suite.get("status").and_then(Value::as_str) == Some("failed")
                || suite
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| !message.is_empty()))
        {
            results.push(result(
                suite_name,
                suite_name,
                RunStatus::InfrastructureError,
                None,
                None,
                Some(
                    suite
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Test suite initialization failed")
                        .into(),
                ),
            ));
        }
    }
    if report
        .get("numRuntimeErrorTestSuites")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
        && !results
            .iter()
            .any(|result| result.status == RunStatus::InfrastructureError)
    {
        results.push(result(
            "runtime-error",
            "Runner reported suite initialization errors",
            RunStatus::InfrastructureError,
            None,
            None,
            None,
        ));
    }
    if report.get("success").and_then(Value::as_bool) == Some(false)
        && !results.is_empty()
        && results
            .iter()
            .all(|result| matches!(result.status, RunStatus::Passed | RunStatus::Skipped))
    {
        results.push(result(
            "incomplete-report",
            "Runner reported unsuccessful execution",
            RunStatus::InfrastructureError,
            None,
            None,
            None,
        ));
    }
    let declared = report
        .get("numTotalTests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    ensure!(
        declared <= results.len() as u64,
        "Jest/Vitest report declares more tests than completed assertion records"
    );
    let declared_failures = report
        .get("numFailedTests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    ensure!(
        declared_failures
            <= results
                .iter()
                .filter(|native| native.status == RunStatus::Failed)
                .count() as u64,
        "Jest/Vitest report failure count lacks completed assertion failure records"
    );
    Ok(results)
}

fn parse_junit(text: &str) -> Result<Vec<NativeTestResult>> {
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 500_000,
            ..Default::default()
        },
    )
    .context("Malformed or unsupported JUnit XML report")?;
    ensure!(
        matches!(
            document.root_element().tag_name().name(),
            "testsuite" | "testsuites"
        ),
        "Report root is not JUnit testsuite/testsuites"
    );
    let mut results = Vec::new();
    for case in document
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
    {
        let name = case
            .attribute("name")
            .filter(|name| !name.is_empty())
            .context("JUnit testcase is missing its name")?;
        let class = case.attribute("classname").unwrap_or_default();
        let failure = case.children().find(|node| node.has_tag_name("failure"));
        let error = case.children().find(|node| node.has_tag_name("error"));
        let skipped = case.children().any(|node| node.has_tag_name("skipped"));
        let status = if failure.is_some() || error.is_some() {
            RunStatus::Failed
        } else if skipped {
            RunStatus::Skipped
        } else {
            RunStatus::Passed
        };
        let message = failure.or(error).map(|node| {
            let kind = if failure.is_some() {
                "Assertion failure"
            } else {
                "Native test error"
            };
            let details = node
                .descendants()
                .filter_map(|node| node.text().filter(|_| node.is_text()))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{kind}: {}\n{details}",
                node.attribute("message").unwrap_or_default()
            )
        });
        results.push(result(
            &format!("{class}::{name}"),
            name,
            status,
            number_ms(
                case.attribute("time").and_then(|value| value.parse().ok()),
                1000.,
            ),
            case.attribute("assertions")
                .and_then(|value| value.parse().ok()),
            message,
        ));
    }
    let declared = document
        .descendants()
        .filter(|node| node.is_element())
        .filter_map(|node| {
            node.attribute("tests")
                .and_then(|value| value.parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0);
    ensure!(
        declared <= results.len(),
        "JUnit report declares more tests than its completed testcase records"
    );
    let declared_failures = document
        .descendants()
        .filter(|node| node.is_element())
        .map(|node| {
            ["failures", "errors"]
                .iter()
                .filter_map(|attribute| {
                    node.attribute(*attribute)
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    ensure!(
        declared_failures
            <= results
                .iter()
                .filter(|native| native.status == RunStatus::Failed)
                .count(),
        "JUnit report failure/error count lacks completed failing testcase records"
    );
    Ok(results)
}

fn parse_tap(text: &str) -> Result<Vec<NativeTestResult>> {
    ensure!(
        text.lines().any(|line| line.trim() == "TAP version 13"),
        "Output is not a Node TAP 13 report"
    );
    let mut results = Vec::new();
    let mut active: Option<usize> = None;
    let mut top_level_rows = 0_u64;
    for line in text.lines() {
        ensure!(
            !line.trim_start().starts_with("Bail out!"),
            "Node TAP runner bailed out"
        );
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("ok ")
            .or_else(|| trimmed.strip_prefix("not ok "))
        {
            if line.len() == trimmed.len() {
                top_level_rows += 1;
            }
            let passed = !trimmed.starts_with("not ok ");
            let (number, name) = rest
                .split_once(" - ")
                .unwrap_or((rest, "Unnamed native test"));
            ensure!(
                number
                    .split_whitespace()
                    .next()
                    .is_some_and(|number| number.parse::<u64>().is_ok()),
                "Invalid TAP test number"
            );
            let skipped = name.contains("# SKIP") || name.contains("# TODO");
            let name = name.split(" # ").next().unwrap_or(name);
            results.push(result(
                &format!("{}:{number}:{name}", line.len() - trimmed.len()),
                name,
                if skipped {
                    RunStatus::Skipped
                } else if passed {
                    RunStatus::Passed
                } else {
                    RunStatus::Failed
                },
                None,
                None,
                None,
            ));
            active = Some(results.len() - 1);
        } else if let Some(index) = active {
            let result = &mut results[index];
            if let Some(value) = trimmed.strip_prefix("duration_ms:") {
                result.duration_ms = number_ms(value.trim().parse().ok(), 1.);
            }
            if trimmed.starts_with("error:")
                || trimmed.starts_with("failureType:")
                || trimmed.starts_with("code:")
            {
                let message = result.message.get_or_insert_with(String::new);
                if message.len() < 16_384 {
                    message.push_str(trimmed);
                    message.push('\n');
                }
                if [
                    "ERR_MODULE_NOT_FOUND",
                    "MODULE_NOT_FOUND",
                    "ERR_UNKNOWN_FILE_EXTENSION",
                    "ERR_PACKAGE_PATH_NOT_EXPORTED",
                ]
                .iter()
                .any(|error| trimmed.contains(error))
                {
                    result.status = RunStatus::InfrastructureError;
                }
            }
            if trimmed == "type: 'suite'" || trimmed == "type: suite" {
                result.native_id.push_str(":suite");
            }
        }
    }
    results.retain(|result| !result.native_id.ends_with(":suite"));
    // Validate completed top-level records before omitting suite wrappers.
    let plans: Vec<u64> = text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("1..")
                .and_then(|value| value.trim().parse().ok())
        })
        .collect();
    ensure!(
        plans.len() == 1 && plans[0] == top_level_rows,
        "Node TAP report has an incomplete or inconsistent top-level plan"
    );
    Ok(results)
}

fn parse_playwright(text: &str) -> Result<Vec<NativeTestResult>> {
    let report: Value = serde_json::from_str(text).context("Malformed Playwright JSON report")?;
    let suites = report
        .get("suites")
        .and_then(Value::as_array)
        .context("Playwright report has no suites array")?;
    let mut results = Vec::new();
    let mut pending: Vec<&Value> = suites.iter().collect();
    while let Some(suite) = pending.pop() {
        if let Some(children) = suite.get("suites").and_then(Value::as_array) {
            pending.extend(children);
        }
        for spec in suite
            .get("specs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let title = spec
                .get("title")
                .and_then(Value::as_str)
                .context("Playwright spec is missing its title")?;
            let file = spec.get("file").and_then(Value::as_str).unwrap_or_default();
            for test in spec
                .get("tests")
                .and_then(Value::as_array)
                .context("Playwright spec is missing its tests")?
            {
                let attempt = test
                    .get("results")
                    .and_then(Value::as_array)
                    .and_then(|results| results.last());
                let status = match test.get("status").and_then(Value::as_str) {
                    Some("expected" | "flaky") => match attempt
                        .and_then(|value| value.get("status"))
                        .and_then(Value::as_str)
                    {
                        Some("passed") => RunStatus::Passed,
                        Some("skipped") => RunStatus::Skipped,
                        // Expected failures retain the native outcome; they are not assertion passes.
                        Some("failed") => RunStatus::Failed,
                        _ => RunStatus::InfrastructureError,
                    },
                    Some("skipped") => RunStatus::Skipped,
                    Some("unexpected") => attempt
                        .and_then(|attempt| attempt.get("status"))
                        .and_then(Value::as_str)
                        .map(native_status)
                        .unwrap_or(RunStatus::Failed),
                    _ => RunStatus::InfrastructureError,
                };
                let project = test
                    .get("projectName")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = attempt
                    .and_then(|attempt| attempt.get("error"))
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                results.push(result(
                    &format!("{file}:{project}:{title}"),
                    title,
                    status,
                    number_ms(
                        attempt
                            .and_then(|attempt| attempt.get("duration"))
                            .and_then(Value::as_f64),
                        1.,
                    ),
                    None,
                    message,
                ));
            }
        }
    }
    for error in report
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        results.push(result(
            "runner-error",
            "Playwright runner error",
            RunStatus::InfrastructureError,
            None,
            None,
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ));
    }
    Ok(results)
}

fn parse_mocha(text: &str) -> Result<Vec<NativeTestResult>> {
    let report: Value = serde_json::from_str(text).context("Malformed Mocha JSON report")?;
    let tests = report
        .get("tests")
        .and_then(Value::as_array)
        .context("Mocha report has no tests array")?;
    let skipped: BTreeSet<_> = report
        .get("pending")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|test| test.get("fullTitle").and_then(Value::as_str))
        .collect();
    let mut results = Vec::new();
    let mut names = BTreeSet::new();
    for test in tests {
        let name = test
            .get("fullTitle")
            .or_else(|| test.get("title"))
            .and_then(Value::as_str)
            .context("Mocha test is missing its title")?;
        names.insert(name);
        let error = test
            .get("err")
            .and_then(Value::as_object)
            .filter(|error| !error.is_empty());
        let status = if skipped.contains(name) {
            RunStatus::Skipped
        } else if error.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Passed
        };
        let message = error
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        results.push(result(
            name,
            name,
            status,
            number_ms(test.get("duration").and_then(Value::as_f64), 1.),
            None,
            message,
        ));
    }
    for failure in report
        .get("failures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = failure
            .get("fullTitle")
            .and_then(Value::as_str)
            .unwrap_or("Mocha hook failure");
        if names.contains(name) {
            for native in results.iter_mut().filter(|native| native.name == name) {
                native.status = RunStatus::Failed;
            }
        } else {
            results.push(result(
                name,
                name,
                RunStatus::InfrastructureError,
                None,
                None,
                failure
                    .get("err")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ));
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_distinguishes_assertions_errors_and_skips_and_refuses_partial_xml() {
        let results = parse_junit(r#"<testsuite tests="4"><testcase name="ok" assertions="2" time="0.01"/><testcase name="assert"><failure message="wrong">expected</failure></testcase><testcase name="runtime"><error message="missing dependency"/></testcase><testcase name="later"><skipped/></testcase></testsuite>"#).unwrap();
        assert_eq!(
            results
                .iter()
                .map(|native| native.status)
                .collect::<Vec<_>>(),
            [
                RunStatus::Passed,
                RunStatus::Failed,
                RunStatus::Failed,
                RunStatus::Skipped
            ]
        );
        assert_eq!(results[0].assertions, Some(2));
        assert_eq!(results[0].duration_ms, Some(10));
        assert!(
            results[1]
                .message
                .as_ref()
                .unwrap()
                .contains("Assertion failure")
        );
        assert!(
            results[2]
                .message
                .as_ref()
                .unwrap()
                .contains("Native test error")
        );
        assert!(
            parse_junit("<testsuite tests=\"2\"><testcase name=\"one\"/></testsuite>").is_err()
        );
        assert!(parse_junit("<!DOCTYPE testsuite [<!ENTITY x 'expanded'>]><testsuite><testcase name='&x;'/></testsuite>").is_err());
    }

    #[test]
    fn tap_requires_complete_plan_and_distinguishes_loader_errors() {
        let report = "TAP version 13\nok 1 - works\nnot ok 2 - import\n  ---\n  code: 'ERR_MODULE_NOT_FOUND'\n  ...\n1..2\n";
        let parsed = parse_tap(report).unwrap();
        assert_eq!(parsed[1].status, RunStatus::InfrastructureError);
        assert!(parse_tap("TAP version 13\nok 1 - works\n1..2\n").is_err());
        assert!(parse_tap("TAP version 13\nok 1 - works\n").is_err());
        assert!(parse_tap("TAP version 13\nok 1 - works\nBail out! crash\n1..1\n").is_err());
    }

    #[test]
    fn jest_runtime_failure_cannot_be_green() {
        let parsed = parse_jest(r#"{"success":false,"testResults":[{"name":"suite","status":"failed","assertionResults":[],"message":"Cannot find module"}]}"#).unwrap();
        assert_eq!(parsed[0].status, RunStatus::InfrastructureError);
        let parsed = parse_jest(r#"{"success":false,"testResults":[{"assertionResults":[{"fullName":"fails","status":"failed","failureMessages":["wrong"]}]}]}"#).unwrap();
        assert_eq!(parsed[0].status, RunStatus::Failed);
        assert!(parse_jest("{}").is_err());
    }

    #[test]
    fn playwright_expected_label_requires_terminal_attempt() {
        let parsed = parse_playwright(r#"{"suites":[{"specs":[{"title":"unfinished","tests":[{"status":"expected","results":[]}]}]}]}"#).unwrap();
        assert_eq!(parsed[0].status, RunStatus::InfrastructureError);
    }

    #[test]
    fn mocha_failure_list_cannot_hide_behind_empty_test_error() {
        let parsed = parse_mocha(r#"{"tests":[{"fullTitle":"fails","err":{}}],"failures":[{"fullTitle":"fails","err":{"message":"assertion"}}]}"#).unwrap();
        assert_eq!(parsed[0].status, RunStatus::Failed);
    }

    fn node_records(root: &Path, file: &str) -> (Project, Framework, TestCandidate) {
        let project = Project {
            id: "fixture-project".into(),
            name: "Fixture".into(),
            path: root.into(),
            created_at: "fixture".into(),
        };
        let framework = Framework {
            id: stable_id(&[&project.id, ".", "Node test"]),
            package_path: ".".into(),
            name: "Node test".into(),
            language: "JavaScript".into(),
            evidence: vec![],
            configurations: vec![],
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
    fn planner_keeps_shell_metacharacters_as_literal_argv_and_rejects_foreign_records() {
        let project_dir = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let artifact_root = artifacts.path().canonicalize().unwrap();
        let filename = "literal;$(touch NEVER).test.mjs";
        fs::write(
            root.join(filename),
            "import {test} from 'node:test'; test('pass',()=>{});",
        )
        .unwrap();
        let (project, framework, candidate) = node_records(&root, filename);
        let command = plan_command(
            &project,
            &framework,
            std::slice::from_ref(&candidate),
            &artifact_root,
        )
        .unwrap();
        assert_eq!(
            command.args.last(),
            root.join(filename).to_str().map(str::to_owned).as_ref()
        );
        assert!(!root.join("NEVER").exists());
        assert!(plan_command(&project, &framework, &[], &artifact_root).is_err());
        assert!(
            plan_command(
                &project,
                &framework,
                std::slice::from_ref(&candidate),
                &root.join("reports")
            )
            .is_err()
        );
        let mut foreign = candidate.clone();
        foreign.id = "forged".into();
        assert!(plan_command(&project, &framework, &[foreign], &artifact_root).is_err());
        let mut traversal = candidate;
        traversal.file = "../escape.test.mjs".into();
        traversal.id = stable_id(&[&project.id, &framework.id, "../escape.test.mjs"]);
        assert!(plan_command(&project, &framework, &[traversal], &artifact_root).is_err());
    }

    #[test]
    fn vitest_substring_collisions_block_before_runner_execution() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::write(root.join("selected.test.js"), "").unwrap();
        let selected = vec![root.join("selected.test.js")];
        assert!(reject_vitest_ambiguous_filters(&root, &root, &selected).is_ok());
        fs::write(root.join("selected.test.jsx"), "").unwrap();
        assert!(reject_vitest_ambiguous_filters(&root, &root, &selected).is_err());
    }

    #[test]
    fn node_zero_cases_and_nonzero_exit_cannot_pass() {
        let project_dir = tempfile::tempdir().unwrap();
        let root = project_dir.path().canonicalize().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        fs::write(root.join("empty.test.mjs"), "").unwrap();
        let (project, framework, candidate) = node_records(&root, "empty.test.mjs");
        let command = plan_command(
            &project,
            &framework,
            &[candidate],
            &artifacts.path().canonicalize().unwrap(),
        )
        .unwrap();
        let text = format!(
            "TAP version 13\nok 1 - {}\n1..1\n",
            root.join("empty.test.mjs").display()
        );
        let empty = parse_results(&command, &text, "", Some(0)).unwrap();
        assert_eq!(empty[0].status, RunStatus::Blocked);
        let mismatch = parse_results(
            &command,
            "TAP version 13\nok 1 - named case\n1..1\n",
            "",
            Some(1),
        )
        .unwrap();
        assert_eq!(mismatch[1].status, RunStatus::InfrastructureError);
        let loader =
            parse_results(&command, &text.replacen("ok 1", "not ok 1", 1), "", Some(1)).unwrap();
        assert_eq!(loader[0].status, RunStatus::InfrastructureError);
    }

    #[test]
    fn bounded_reader_refuses_growth_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("report.json");
        fs::write(&file, "12345").unwrap();
        assert!(read_bounded(&file, 4).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&file, root.join("linked.json")).unwrap();
            assert!(read_bounded(&root.join("linked.json"), 100).is_err());
        }
    }
}
