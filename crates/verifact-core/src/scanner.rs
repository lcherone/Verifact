//! Deterministic, passive discovery. Project configuration is data, never code:
//! this module does not spawn processes, load JavaScript, or run test runners.
use crate::{Framework, Project, ScanReport, ScanWarning, TestCandidate, stable_id, timestamp};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const VISION_LOOP_PACKAGE: &str = ".vscode/vision-loop";
const VISION_LOOP_MANIFEST: &str = ".vscode/vision-loop/package.json";

#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Directory basenames excluded throughout the tree, in addition to defaults
    /// supplied by `Default`. Symlinks are always skipped.
    pub excluded_directories: Vec<String>,
    pub max_files: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            excluded_directories: [
                "node_modules",
                "vendor",
                ".git",
                "dist",
                "build",
                "coverage",
                ".cache",
                ".tools",
                "target",
                ".next",
                ".nuxt",
                ".idea",
                ".vscode",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_files: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    PhpUnit,
    Pest,
    Codeception,
    ComposerScript,
    Playwright,
    Cypress,
    Vitest,
    Jest,
    Mocha,
    NodeTest,
    NpmScript,
    VisionLoop,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::PhpUnit => "PHPUnit",
            Self::Pest => "Pest",
            Self::Codeception => "Codeception",
            Self::ComposerScript => "Composer script",
            Self::Playwright => "Playwright",
            Self::Cypress => "Cypress",
            Self::Vitest => "Vitest",
            Self::Jest => "Jest",
            Self::Mocha => "Mocha",
            Self::NodeTest => "Node test",
            Self::NpmScript => "npm script",
            Self::VisionLoop => "Vision Loop",
        }
    }

    fn language(self) -> &'static str {
        match self {
            Self::PhpUnit | Self::Pest | Self::Codeception | Self::ComposerScript => "PHP",
            _ => "JavaScript/TypeScript",
        }
    }

    fn suggested_command(self) -> Option<Vec<String>> {
        let args: &[&str] = match self {
            Self::PhpUnit => &["vendor/bin/phpunit"],
            Self::Pest => &["vendor/bin/pest"],
            Self::Codeception => &["vendor/bin/codecept", "run"],
            Self::Playwright => &["node_modules/.bin/playwright", "test"],
            Self::Cypress => &["node_modules/.bin/cypress", "run"],
            Self::Vitest => &["node_modules/.bin/vitest", "run"],
            Self::Jest => &["node_modules/.bin/jest", "--runInBand"],
            Self::Mocha => &["node_modules/.bin/mocha"],
            Self::NodeTest => &["node", "--test"],
            Self::ComposerScript | Self::NpmScript | Self::VisionLoop => return None,
        };
        Some(args.iter().map(|arg| (*arg).to_owned()).collect())
    }
}

struct Registry<'a> {
    project: &'a Project,
    frameworks: BTreeMap<(PathBuf, Kind), Framework>,
}

impl Registry<'_> {
    fn add(&mut self, package: &Path, kind: Kind, evidence: String, config: Option<&Path>) {
        let key = (package.to_owned(), kind);
        let framework = self.frameworks.entry(key).or_insert_with(|| Framework {
            id: stable_id(&[&self.project.id, &package.to_string_lossy(), kind.name()]),
            package_path: package.to_owned(),
            name: kind.name().to_owned(),
            language: kind.language().to_owned(),
            evidence: Vec::new(),
            configurations: Vec::new(),
            suggested_command: kind.suggested_command(),
        });
        if !framework.evidence.contains(&evidence) {
            framework.evidence.push(evidence);
        }
        if let Some(config) = config {
            let config = config.to_owned();
            if !framework.configurations.contains(&config) {
                framework.configurations.push(config);
            }
        }
    }

    fn kinds(&self, package: &Path, php: bool) -> Vec<Kind> {
        self.frameworks
            .keys()
            .filter(|(owner, kind)| owner == package && (kind.language() == "PHP") == php)
            .map(|(_, kind)| *kind)
            .collect()
    }
}

struct CandidateInfo {
    file: PathBuf,
    package: PathBuf,
    php: bool,
    explicit: Vec<Kind>,
}

/// Inspect a local project without executing any project-controlled code.
/// Returned candidates are files, never confirmed individual test cases.
/// Nested manifest directories create ownership boundaries even if malformed.
pub fn scan_project(project: &Project, options: &ScanOptions) -> Result<ScanReport> {
    ensure!(
        options.max_files > 0,
        "The scanner file limit must be greater than zero"
    );
    let root = project
        .path
        .canonicalize()
        .context("Cannot resolve project directory")?;
    ensure!(root.is_dir(), "Project path is not a directory");
    let mut warnings = Vec::new();
    let files = inventory(&root, options, &mut warnings)?;
    let mut packages = BTreeSet::from([PathBuf::from(".")]);
    for file in &files {
        if matches!(
            file.file_name().and_then(|name| name.to_str()),
            Some("package.json" | "composer.json")
        ) {
            packages.insert(relative_parent(file));
        }
    }
    let mut registry = Registry {
        project,
        frameworks: BTreeMap::new(),
    };
    for file in &files {
        let package = owner_of(file, &packages);
        if file == Path::new(VISION_LOOP_MANIFEST) {
            // This file is only admitted by the narrow, validated inventory
            // adapter. Vision Loop depends on Playwright but is not Playwright
            // Test: never infer its runner from that dependency declaration.
            registry.add(
                &package,
                Kind::VisionLoop,
                format!("{} identifies a Vision Loop harness", file.display()),
                None,
            );
            continue;
        }
        match file.file_name().and_then(|name| name.to_str()) {
            Some("package.json") => {
                detect_manifest(&root, file, &package, false, &mut registry, &mut warnings)
            }
            Some("composer.json") => {
                detect_manifest(&root, file, &package, true, &mut registry, &mut warnings)
            }
            _ => detect_config(file, &package, &mut registry),
        }
    }
    let mut possible = Vec::new();
    for file in &files {
        let php = is_php_candidate(file);
        if !php && !is_node_candidate(file) {
            continue;
        }
        let package = owner_of(file, &packages);
        let text = read_text(&root, file, &mut warnings).unwrap_or_default();
        let explicit = explicit_kinds(file, &text, php);
        for kind in &explicit {
            registry.add(
                &package,
                *kind,
                format!(
                    "{} contains {} test conventions",
                    file.display(),
                    kind.name()
                ),
                None,
            );
        }
        possible.push(CandidateInfo {
            file: file.clone(),
            package,
            php,
            explicit,
        });
    }
    let mut candidates = Vec::new();
    for candidate in possible {
        let detected = registry.kinds(&candidate.package, candidate.php);
        let attributed = attribute_candidate(&candidate, &detected);
        match attributed {
            Some(kind) if candidate_matches_kind(&candidate.file, kind, &candidate.explicit) => {
                if let Some(framework) = registry.frameworks.get(&(candidate.package.clone(), kind)) {
                    candidates.push(TestCandidate {
                        id: stable_id(&[&project.id, &framework.id, &candidate.file.to_string_lossy()]),
                        framework_id: framework.id.clone(),
                        display_name: candidate.file.to_string_lossy().into_owned(),
                        file: candidate.file,
                        discovery_kind: "file_candidate".to_owned(),
                    });
                }
            }
            None if !detected.is_empty() => warnings.push(ScanWarning {
                path: candidate.file,
                message: "Test-like file has ambiguous framework ownership; it was not counted more than once or assigned arbitrarily".to_owned(),
            }),
            _ => {}
        }
    }
    add_vision_loop_candidates(&root, &files, &mut registry, &mut candidates, &mut warnings);
    let mut frameworks: Vec<_> = registry.frameworks.into_values().collect();
    for framework in &mut frameworks {
        framework.evidence.sort();
        framework.configurations.sort();
    }
    frameworks.sort_by(|a, b| (&a.package_path, &a.name).cmp(&(&b.package_path, &b.name)));
    candidates.sort_by(|a, b| (&a.file, &a.framework_id).cmp(&(&b.file, &b.framework_id)));
    warnings.sort_by(|a, b| (&a.path, &a.message).cmp(&(&b.path, &b.message)));
    warnings.dedup();
    Ok(ScanReport {
        project_id: project.id.clone(),
        scanned_at: timestamp(),
        frameworks,
        candidates,
        warnings,
        files_inspected: files.len(),
        revision: read_revision(&root),
        // A passive filesystem scan cannot truthfully determine dirty/untracked state.
        working_tree: None,
    })
}

fn relative_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_owned()
}

fn owner_of(file: &Path, packages: &BTreeSet<PathBuf>) -> PathBuf {
    packages
        .iter()
        .filter(|package| package.as_path() != Path::new(".") && file.starts_with(package))
        .max_by_key(|package| package.components().count())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

fn inventory(
    root: &Path,
    options: &ScanOptions,
    warnings: &mut Vec<ScanWarning>,
) -> Result<Vec<PathBuf>> {
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    let mut entries_seen = 0usize;
    let entry_limit = options.max_files.saturating_mul(4).max(64);
    let mut limited = false;
    inventory_vision_loop(
        root,
        options,
        &mut files,
        &mut entries_seen,
        &mut limited,
        warnings,
    );
    while let Some(relative) = pending.pop() {
        if limited {
            break;
        }
        let directory = root.join(&relative);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if relative.as_os_str().is_empty() => {
                return Err(error).context("Cannot read project directory");
            }
            Err(error) => {
                warnings.push(ScanWarning {
                    path: relative,
                    message: format!("Cannot read directory: {error}"),
                });
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            entries_seen += 1;
            if entries_seen > entry_limit {
                limited = true;
                break;
            }
            match entry {
                Ok(entry) => children.push(entry),
                Err(error) => warnings.push(ScanWarning {
                    path: relative.clone(),
                    message: format!("Cannot inspect directory entry: {error}"),
                }),
            }
        }
        children.sort_by_key(|entry| entry.file_name());
        let mut subdirectories = Vec::new();
        for entry in children {
            let path = relative.join(entry.file_name());
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    warnings.push(ScanWarning {
                        path,
                        message: format!("Cannot inspect file type: {error}"),
                    });
                    continue;
                }
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                // Editor-workspace contents remain private even when callers
                // replace the default exclusion list. Only the explicit Vision
                // Loop adapter above can admit known metadata beneath .vscode.
                if entry.file_name() != ".vscode"
                    && !options
                        .excluded_directories
                        .iter()
                        .any(|excluded| entry.file_name() == excluded.as_str())
                {
                    subdirectories.push(path);
                }
            } else if kind.is_file() {
                if files.len() == options.max_files {
                    limited = true;
                    break;
                }
                files.push(path);
            }
        }
        if limited {
            break;
        }
        pending.extend(subdirectories.into_iter().rev());
    }
    if limited {
        warnings.push(ScanWarning { path: PathBuf::from("."), message: format!("Scan limit reached ({} files or {entry_limit} directory entries); discovery is incomplete", options.max_files) });
    }
    files.sort();
    Ok(files)
}

/// Admit only a recognized harness manifest, direct config*.json files and
/// direct suites/*.json files. Never descend into actions, runs, storage,
/// node_modules, or arbitrary neighboring .vscode tools.
fn inventory_vision_loop(
    root: &Path,
    options: &ScanOptions,
    files: &mut Vec<PathBuf>,
    entries_seen: &mut usize,
    limited: &mut bool,
    warnings: &mut Vec<ScanWarning>,
) {
    if options
        .excluded_directories
        .iter()
        .any(|name| name == "vision-loop")
        || !is_local_directory(root, Path::new(VISION_LOOP_PACKAGE))
    {
        return;
    }
    let manifest_path = Path::new(VISION_LOOP_MANIFEST);
    let manifest_absolute = root.join(manifest_path);
    if !fs::symlink_metadata(&manifest_absolute).is_ok_and(|metadata| metadata.is_file()) {
        return;
    }
    let Some(text) = read_text(root, manifest_path, warnings) else {
        return;
    };
    let manifest = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(error) => {
            warnings.push(ScanWarning {
                path: manifest_path.to_owned(),
                message: format!("Malformed Vision Loop package JSON: {error}"),
            });
            return;
        }
    };
    let scripted_harness = manifest
        .get("scripts")
        .and_then(Value::as_object)
        .is_some_and(|scripts| {
            scripts.values().filter_map(Value::as_str).any(|command| {
                command
                    .split(|character: char| {
                        character.is_whitespace() || "'\";|&()".contains(character)
                    })
                    .any(|argument| {
                        Path::new(argument)
                            .file_name()
                            .is_some_and(|name| name == "vision-loop.mjs")
                    })
            })
        });
    if !scripted_harness {
        return;
    }
    files.push(manifest_path.to_owned());
    *entries_seen += 1;
    let entry_limit = options.max_files.saturating_mul(4).max(64);
    for (directory, suites) in [
        (PathBuf::from(VISION_LOOP_PACKAGE), false),
        (Path::new(VISION_LOOP_PACKAGE).join("suites"), true),
    ] {
        if !is_local_directory(root, &directory) {
            continue;
        }
        let entries = match fs::read_dir(root.join(&directory)) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(ScanWarning {
                    path: directory,
                    message: format!("Cannot read Vision Loop metadata directory: {error}"),
                });
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            *entries_seen += 1;
            if *entries_seen > entry_limit {
                *limited = true;
                break;
            }
            match entry {
                Ok(entry) => children.push(entry),
                Err(error) => warnings.push(ScanWarning {
                    path: directory.clone(),
                    message: format!("Cannot inspect Vision Loop directory entry: {error}"),
                }),
            }
        }
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let known_file = if suites {
                name.ends_with(".json")
            } else {
                name.starts_with("config") && name.ends_with(".json")
            };
            if !known_file {
                continue;
            }
            let path = directory.join(name);
            match entry.file_type() {
                Ok(kind) if kind.is_file() => {}
                Ok(_) => continue,
                Err(error) => {
                    warnings.push(ScanWarning {
                        path,
                        message: format!("Cannot inspect Vision Loop metadata type: {error}"),
                    });
                    continue;
                }
            }
            if files.len() == options.max_files {
                *limited = true;
                break;
            }
            files.push(path);
        }
        if *limited {
            break;
        }
    }
}

fn is_local_directory(root: &Path, relative: &Path) -> bool {
    let mut path = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return false;
        };
        path.push(name);
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            return false;
        }
    }
    path.canonicalize().is_ok_and(|path| path.starts_with(root))
}

fn add_vision_loop_candidates(
    root: &Path,
    files: &[PathBuf],
    registry: &mut Registry<'_>,
    candidates: &mut Vec<TestCandidate>,
    warnings: &mut Vec<ScanWarning>,
) {
    #[derive(serde::Deserialize)]
    struct SuiteMetadata {
        // Validate the suite envelope without retaining target/action values.
        #[serde(rename = "targets")]
        _targets: Vec<serde::de::IgnoredAny>,
    }
    let package = Path::new(VISION_LOOP_PACKAGE);
    let suites = package.join("suites");
    if !registry
        .frameworks
        .contains_key(&(package.to_owned(), Kind::VisionLoop))
    {
        return;
    }
    for file in files {
        if file.parent() == Some(package)
            && file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("config") && name.ends_with(".json"))
        {
            // Record the configuration path, not its potentially sensitive values.
            registry.add(
                package,
                Kind::VisionLoop,
                "Vision Loop configuration paths discovered without loading configuration values"
                    .into(),
                Some(file),
            );
        }
        if file.parent() != Some(suites.as_path()) {
            continue;
        }
        let Some(text) = read_text(root, file, warnings) else {
            continue;
        };
        match serde_json::from_str::<SuiteMetadata>(&text) {
            Ok(_) => {}
            Err(error) => {
                warnings.push(ScanWarning {
                    path: file.clone(),
                    // A serde type error can quote the rejected input value.
                    // Preserve location while keeping suite contents private.
                    message: format!("Invalid Vision Loop suite metadata: expected an object with a targets array (line {}, column {}); no target or action was executed", error.line(), error.column()),
                });
                continue;
            }
        }
        if let Some(framework) = registry
            .frameworks
            .get(&(package.to_owned(), Kind::VisionLoop))
        {
            candidates.push(TestCandidate {
                id: stable_id(&[&registry.project.id, &framework.id, &file.to_string_lossy()]),
                framework_id: framework.id.clone(),
                file: file.clone(),
                display_name: file.to_string_lossy().into_owned(),
                discovery_kind: "file_candidate".into(),
            });
        }
    }
}

fn read_text(root: &Path, relative: &Path, warnings: &mut Vec<ScanWarning>) -> Option<String> {
    let result = (|| -> Result<String> {
        let path = root.join(relative);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Symbolic links are not read"
        );
        ensure!(
            path.canonicalize()?.starts_with(root),
            "Path resolves outside the project"
        );
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_TEXT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_TEXT_BYTES,
            "File exceeds the 1 MiB passive inspection limit"
        );
        String::from_utf8(bytes).context("File is not valid UTF-8")
    })();
    match result {
        Ok(text) => Some(text),
        Err(error) => {
            warnings.push(ScanWarning {
                path: relative.to_owned(),
                message: format!("Cannot inspect file contents: {error:#}"),
            });
            None
        }
    }
}

fn detect_manifest(
    root: &Path,
    file: &Path,
    package: &Path,
    php: bool,
    registry: &mut Registry<'_>,
    warnings: &mut Vec<ScanWarning>,
) {
    let Some(text) = read_text(root, file, warnings) else {
        return;
    };
    let manifest = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(_) => {
            warnings.push(ScanWarning {
                path: file.to_owned(),
                message: "Manifest must be a JSON object".into(),
            });
            return;
        }
        Err(error) => {
            warnings.push(ScanWarning {
                path: file.to_owned(),
                message: format!("Malformed JSON manifest: {error}"),
            });
            return;
        }
    };
    let dependencies = if php {
        &["require", "require-dev"][..]
    } else {
        &[
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ][..]
    };
    for section in dependencies {
        if let Some(object) = manifest.get(section).and_then(Value::as_object) {
            for dependency in object.keys() {
                let kind = match dependency.as_str() {
                    "phpunit/phpunit" if php => Some(Kind::PhpUnit),
                    "pestphp/pest" if php => Some(Kind::Pest),
                    "codeception/codeception" if php => Some(Kind::Codeception),
                    "@playwright/test" | "playwright" if !php => Some(Kind::Playwright),
                    "cypress" if !php => Some(Kind::Cypress),
                    "vitest" if !php => Some(Kind::Vitest),
                    "jest" | "@jest/core" if !php => Some(Kind::Jest),
                    "mocha" if !php => Some(Kind::Mocha),
                    _ => None,
                };
                if let Some(kind) = kind {
                    registry.add(
                        package,
                        kind,
                        format!("{} declares {section}.{dependency}", file.display()),
                        Some(file),
                    );
                }
            }
        }
    }
    if let Some(scripts) = manifest.get("scripts").and_then(Value::as_object) {
        for (name, command) in scripts {
            let commands: Vec<&str> = match command {
                Value::String(command) => vec![command.as_str()],
                Value::Array(commands) if php => {
                    commands.iter().filter_map(Value::as_str).collect()
                }
                _ => Vec::new(),
            };
            let mut known = false;
            for command in commands {
                for kind in command_kinds(command, php) {
                    known = true;
                    registry.add(
                        package,
                        kind,
                        format!(
                            "{} scripts.{name} references {}",
                            file.display(),
                            kind.name()
                        ),
                        Some(file),
                    );
                }
            }
            if !known && (name == "test" || name.starts_with("test:")) {
                registry.add(
                    package,
                    if php {
                        Kind::ComposerScript
                    } else {
                        Kind::NpmScript
                    },
                    format!(
                        "{} defines scripts.{name}; runner is not statically identified",
                        file.display()
                    ),
                    Some(file),
                );
            }
        }
    }
    if !php && manifest.get("jest").is_some() {
        registry.add(
            package,
            Kind::Jest,
            format!("{} contains Jest configuration", file.display()),
            Some(file),
        );
    }
}

fn command_kinds(command: &str, php: bool) -> Vec<Kind> {
    let words: Vec<_> = command
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || "-/_@.".contains(character))
        })
        .collect();
    let has = |binary: &str| {
        words
            .iter()
            .any(|word| *word == binary || word.ends_with(&format!("/{binary}")))
    };
    if php {
        [
            ("phpunit", Kind::PhpUnit),
            ("pest", Kind::Pest),
            ("codecept", Kind::Codeception),
        ]
        .into_iter()
        .filter_map(|(binary, kind)| has(binary).then_some(kind))
        .collect()
    } else {
        let mut kinds: Vec<_> = [
            ("playwright", Kind::Playwright),
            ("cypress", Kind::Cypress),
            ("vitest", Kind::Vitest),
            ("jest", Kind::Jest),
            ("mocha", Kind::Mocha),
        ]
        .into_iter()
        .filter_map(|(binary, kind)| has(binary).then_some(kind))
        .collect();
        if has("node") && words.contains(&"--test") {
            kinds.push(Kind::NodeTest);
        }
        kinds
    }
}

fn detect_config(file: &Path, package: &Path, registry: &mut Registry<'_>) {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let kind = if name.starts_with("phpunit")
        && (name.ends_with(".xml") || name.ends_with(".xml.dist") || name.ends_with(".xml.example"))
    {
        Some(Kind::PhpUnit)
    } else if matches!(name, "Pest.php")
        && file
            .components()
            .any(|component| component.as_os_str() == "tests")
    {
        Some(Kind::Pest)
    } else if [
        "codeception.yml",
        "codeception.yaml",
        "codeception.dist.yml",
        "codeception.dist.yaml",
    ]
    .contains(&name)
    {
        Some(Kind::Codeception)
    } else if name.starts_with("playwright.config.") {
        Some(Kind::Playwright)
    } else if name.starts_with("cypress.config.") {
        Some(Kind::Cypress)
    } else if name.starts_with("vitest.config.") {
        Some(Kind::Vitest)
    } else if name.starts_with("jest.config.") {
        Some(Kind::Jest)
    } else if name == ".mocharc" || name.starts_with(".mocharc.") {
        Some(Kind::Mocha)
    } else {
        None
    };
    if let Some(kind) = kind {
        registry.add(
            package,
            kind,
            format!("{} configuration detected", file.display()),
            Some(file),
        );
    }
}

fn is_php_candidate(file: &Path) -> bool {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name != "Pest.php"
        && (name.ends_with("Test.php")
            || name.ends_with("_test.php")
            || name.ends_with("Cest.php")
            || name.ends_with("Cept.php")
            || name.ends_with(".phpt")
            || (name.ends_with(".php")
                && file.components().any(|component| {
                    component.as_os_str() == "Feature" || component.as_os_str() == "Unit"
                })
                && file
                    .components()
                    .any(|component| component.as_os_str() == "tests")))
}

fn is_node_candidate(file: &Path) -> bool {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    ["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"].contains(&extension)
        && (name.contains(".test.")
            || name.contains(".spec.")
            || name.contains(".cy.")
            || name.starts_with("test-")
            || name.ends_with("_test.js")
            || file
                .components()
                .any(|component| component.as_os_str() == "__tests__")
            || file.components().any(|component| {
                component.as_os_str() == "test" || component.as_os_str() == "tests"
            }) && !name.contains(".config."))
}

fn explicit_kinds(file: &Path, text: &str, php: bool) -> Vec<Kind> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if php {
        if name.ends_with("Cest.php")
            || name.ends_with("Cept.php")
            || text.contains("Codeception\\")
        {
            return vec![Kind::Codeception];
        }
        if text.contains("Pest\\")
            || has_named_test_call(text, "test")
            || has_named_test_call(text, "it")
        {
            return vec![Kind::Pest];
        }
        if text.contains("PHPUnit\\") || text.contains("PHPUnit_Framework_TestCase") {
            return vec![Kind::PhpUnit];
        }
        return Vec::new();
    }
    let mut kinds = Vec::new();
    for (needle, kind) in [
        ("@playwright/test", Kind::Playwright),
        ("vitest", Kind::Vitest),
        ("@jest/globals", Kind::Jest),
        ("node:test", Kind::NodeTest),
    ] {
        if text.contains(&format!("'{needle}'")) || text.contains(&format!("\"{needle}\"")) {
            kinds.push(kind);
        }
    }
    if name.contains(".cy.")
        || file
            .components()
            .any(|component| component.as_os_str() == "cypress")
    {
        kinds.push(Kind::Cypress);
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

fn attribute_candidate(candidate: &CandidateInfo, detected: &[Kind]) -> Option<Kind> {
    if candidate.explicit.len() == 1 {
        return candidate.explicit.first().copied();
    }
    if candidate.explicit.len() > 1 {
        return None;
    }
    let mut runners: Vec<_> = detected
        .iter()
        .copied()
        .filter(|kind| !matches!(kind, Kind::ComposerScript | Kind::NpmScript))
        .collect();
    if candidate.php && runners.contains(&Kind::Pest) {
        // Pest depends on PHPUnit but owns the PHP test-file inventory in a Pest package.
        runners.retain(|kind| *kind != Kind::PhpUnit);
    }
    if runners.len() > 1
        && !candidate.php
        && runners.contains(&Kind::Playwright)
        && candidate.file.components().any(|component| {
            component.as_os_str() == "e2e" || component.as_os_str() == "playwright"
        })
    {
        return Some(Kind::Playwright);
    }
    match runners.as_slice() {
        [kind] => Some(*kind),
        [] if detected.len() == 1 => detected.first().copied(),
        _ => None,
    }
}

fn candidate_matches_kind(file: &Path, kind: Kind, explicit: &[Kind]) -> bool {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if kind.language() == "PHP" {
        return name.ends_with("Test.php")
            || name.ends_with("_test.php")
            || name.ends_with("Cest.php")
            || name.ends_with("Cept.php")
            || name.ends_with(".phpt")
            || !explicit.is_empty();
    }
    // A directory named tests does not make every helper/setup module a test
    // for Jest, Vitest or Playwright. Preserve their conventional file shapes.
    if name.contains(".test.") || name.contains(".spec.") || name.contains(".cy.") {
        return true;
    }
    match kind {
        Kind::Jest => file
            .components()
            .any(|component| component.as_os_str() == "__tests__"),
        Kind::Mocha => file
            .components()
            .any(|component| component.as_os_str() == "test"),
        Kind::NodeTest => {
            name.starts_with("test-")
                || name.ends_with("_test.js")
                || file
                    .components()
                    .any(|component| component.as_os_str() == "test")
        }
        _ => false,
    }
}

fn has_named_test_call(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(index, _)| {
        let previous = text[..index].chars().next_back();
        if previous
            .is_some_and(|character| character.is_alphanumeric() || "_$>".contains(character))
        {
            return false;
        }
        let tail = text[index + name.len()..].trim_start();
        tail.strip_prefix('(')
            .is_some_and(|arguments| arguments.trim_start().starts_with(['\'', '"']))
    })
}

fn read_revision(root: &Path) -> Option<String> {
    let git = root.join(".git");
    if !fs::symlink_metadata(&git).ok()?.file_type().is_dir() {
        return None;
    }
    let read_git = |relative: &str| -> Option<String> {
        let path = git.join(relative);
        if fs::symlink_metadata(&path).ok()?.file_type().is_symlink() {
            return None;
        }
        if !path.canonicalize().ok()?.starts_with(&git) {
            return None;
        }
        let mut text = String::new();
        File::open(path)
            .ok()?
            .take(MAX_TEXT_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    };
    let head = read_git("HEAD")?;
    let head = head.trim();
    let valid_hash = |value: &str| {
        (value.len() == 40 || value.len() == 64)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if valid_hash(head) {
        return Some(head.to_owned());
    }
    let reference = head.strip_prefix("ref: ")?;
    if !reference.starts_with("refs/")
        || Path::new(reference)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    if let Some(value) = read_git(reference)
        && valid_hash(value.trim())
    {
        return Some(value.trim().to_owned());
    }
    read_git("packed-refs")?
        .lines()
        .filter(|line| !line.starts_with(['#', '^']))
        .find_map(|line| {
            let (hash, name) = line.split_once(' ')?;
            (name == reference && valid_hash(hash)).then(|| hash.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project(root: &Path) -> Project {
        Project {
            id: "test-project".into(),
            name: "Scanner test".into(),
            path: root.to_owned(),
            created_at: "test".into(),
        }
    }
    fn write(root: &Path, file: &str, contents: &str) {
        let file = root.join(file);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
    fn report(root: &Path) -> ScanReport {
        scan_project(&project(root), &ScanOptions::default()).unwrap()
    }

    #[test]
    fn nested_packages_keep_framework_ownership_and_stable_ids() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(
            dir.path(),
            "packages/a/package.json",
            r#"{"devDependencies":{"vitest":"1"}}"#,
        );
        write(
            dir.path(),
            "packages/b/package.json",
            r#"{"devDependencies":{"jest":"1"}}"#,
        );
        write(
            dir.path(),
            "packages/a/src/counter.test.ts",
            "import { test } from 'vitest';",
        );
        write(
            dir.path(),
            "packages/b/src/counter.test.ts",
            "test('counter', () => {});",
        );
        write(
            dir.path(),
            "node_modules/ignored/package.json",
            r#"{"devDependencies":{"mocha":"1"}}"#,
        );
        let first = report(dir.path());
        let second = report(dir.path());
        assert_eq!(first.frameworks.len(), 2);
        assert_eq!(first.candidates.len(), 2);
        assert_eq!(first.candidates, second.candidates);
        for candidate in &first.candidates {
            let framework = first
                .frameworks
                .iter()
                .find(|framework| framework.id == candidate.framework_id)
                .unwrap();
            assert!(candidate.file.starts_with(&framework.package_path));
            assert_eq!(candidate.discovery_kind, "file_candidate");
        }
    }

    #[test]
    fn malformed_manifests_are_warnings_and_create_package_boundaries() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"devDependencies":{"vitest":"1"}}"#,
        );
        write(dir.path(), "nested/package.json", "{bad");
        write(
            dir.path(),
            "nested/foo.test.ts",
            "test('unknown', () => {});",
        );
        let result = report(dir.path());
        assert_eq!(result.frameworks.len(), 1);
        assert!(result.candidates.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.path == Path::new("nested/package.json")
                    && warning.message.contains("Malformed JSON"))
        );
    }

    #[test]
    fn ambiguous_files_are_not_duplicated_and_explicit_imports_disambiguate() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"devDependencies":{"vitest":"1","jest":"1"}}"#,
        );
        write(
            dir.path(),
            "tests/ambiguous.test.js",
            "test('ambiguous', () => {});",
        );
        write(
            dir.path(),
            "tests/clear.test.js",
            "import { test } from 'vitest';",
        );
        let result = report(dir.path());
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].file, Path::new("tests/clear.test.js"));
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.path == Path::new("tests/ambiguous.test.js"))
        );
    }

    #[test]
    fn mixed_php_node_frameworks_share_root_without_duplicate_php_files() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "composer.json",
            r#"{"require-dev":{"pestphp/pest":"1","phpunit/phpunit":"1"}}"#,
        );
        write(
            dir.path(),
            "package.json",
            r#"{"devDependencies":{"@playwright/test":"1"}}"#,
        );
        write(dir.path(), "tests/Pest.php", "<?php uses(TestCase::class);");
        write(
            dir.path(),
            "tests/Feature/CheckoutTest.php",
            "<?php test('checkout', fn() => true);",
        );
        write(
            dir.path(),
            "e2e/checkout.spec.ts",
            "import { test } from '@playwright/test';",
        );
        let result = report(dir.path());
        assert_eq!(result.frameworks.len(), 3);
        assert_eq!(result.candidates.len(), 2);
        let php = result
            .candidates
            .iter()
            .find(|candidate| {
                candidate
                    .file
                    .extension()
                    .is_some_and(|extension| extension == "php")
            })
            .unwrap();
        assert_eq!(
            result
                .frameworks
                .iter()
                .find(|framework| framework.id == php.framework_id)
                .unwrap()
                .name,
            "Pest"
        );
    }

    #[test]
    fn manifests_and_configs_are_never_executed() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"scripts":{"test":"touch was-executed"}}"#,
        );
        write(
            dir.path(),
            "vitest.config.ts",
            "throw new Error('must never load this');",
        );
        write(
            dir.path(),
            "tests/example.test.ts",
            "test('example', () => {});",
        );
        let result = report(dir.path());
        assert_eq!(result.candidates.len(), 1);
        assert!(!dir.path().join("was-executed").exists());
    }

    #[test]
    fn test_directory_helpers_do_not_inflate_vitest_file_candidates() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{"devDependencies":{"vitest":"1"}}"#,
        );
        write(
            dir.path(),
            "tests/helpers.ts",
            "import { vi } from 'vitest'; export const clock = vi.fn();",
        );
        write(
            dir.path(),
            "tests/price.test.ts",
            "import { test } from 'vitest'; test('price', () => {});",
        );
        let result = report(dir.path());
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].file, Path::new("tests/price.test.ts"));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn ordinary_php_calls_do_not_misidentify_pest() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "composer.json",
            r#"{"require-dev":{"phpunit/phpunit":"1"}}"#,
        );
        write(
            dir.path(),
            "tests/CheckoutTest.php",
            "<?php submit('order'); $test('value');",
        );
        let result = report(dir.path());
        assert_eq!(result.frameworks.len(), 1);
        assert_eq!(result.frameworks[0].name, "PHPUnit");
        assert_eq!(result.candidates.len(), 1);
    }

    #[test]
    fn traversal_and_content_are_bounded() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            &" ".repeat(MAX_TEXT_BYTES as usize + 1),
        );
        assert!(
            report(dir.path())
                .warnings
                .iter()
                .any(|warning| warning.message.contains("1 MiB"))
        );
        write(dir.path(), "a.txt", "a");
        write(dir.path(), "b.txt", "b");
        let options = ScanOptions {
            max_files: 1,
            ..Default::default()
        };
        let result = scan_project(&project(dir.path()), &options).unwrap();
        assert_eq!(result.files_inspected, 1);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Scan limit reached"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_directories_and_files_are_not_followed() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write(
            outside.path(),
            "package.json",
            r#"{"devDependencies":{"vitest":"1"}}"#,
        );
        write(
            outside.path(),
            "secret.test.ts",
            "import { test } from 'vitest';",
        );
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("package.json"),
            dir.path().join("package.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("cycle")).unwrap();
        let result = report(dir.path());
        assert_eq!(result.files_inspected, 0);
        assert!(result.frameworks.is_empty());
    }

    #[test]
    fn git_revision_is_read_without_hooks_or_subprocesses() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".git/HEAD", "ref: refs/heads/main\n");
        write(
            dir.path(),
            ".git/refs/heads/main",
            "1234567890123456789012345678901234567890\n",
        );
        let result = report(dir.path());
        assert_eq!(
            result.revision.as_deref(),
            Some("1234567890123456789012345678901234567890")
        );
        assert_eq!(result.working_tree, None);
        assert_eq!(result.files_inspected, 0);
    }
}
