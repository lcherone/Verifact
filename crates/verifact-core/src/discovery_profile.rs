//! Saved passive discovery policy and evidence-led project inspection.
use crate::{Core, Project, ScanOptions, ScanWarning, timestamp};
use anyhow::{Context, Result, ensure};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ScanProfile {
    /// Project-relative candidate-file globs. Empty includes all candidates.
    /// Package manifests/configuration remain available to preserve ownership.
    pub include_globs: Vec<String>,
    /// Excludes content reads and matching directory subtrees. A manifest's
    /// filename can still establish an ownership boundary without being read.
    pub exclude_globs: Vec<String>,
    pub excluded_directories: Vec<String>,
    pub max_files: usize,
}
impl Default for ScanProfile {
    fn default() -> Self {
        let options = ScanOptions::default();
        Self {
            include_globs: vec![],
            exclude_globs: vec![],
            excluded_directories: options.excluded_directories,
            max_files: options.max_files,
        }
    }
}
impl ScanProfile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=1_000_000).contains(&self.max_files),
            "Scan max_files must be between 1 and 1000000"
        );
        ensure!(
            self.excluded_directories.len() <= 128,
            "At most 128 directory exclusions are supported"
        );
        for directory in &self.excluded_directories {
            ensure!(
                !directory.is_empty()
                    && directory.len() <= 128
                    && !matches!(directory.as_str(), "." | "..")
                    && !directory.contains(['/', '\\', '\0']),
                "Directory exclusions must be simple directory basenames"
            );
        }
        ScanFilter::new(&self.options())?;
        Ok(())
    }
    pub fn options(&self) -> ScanOptions {
        ScanOptions {
            include_globs: self.include_globs.clone(),
            exclude_globs: self.exclude_globs.clone(),
            excluded_directories: self.excluded_directories.clone(),
            max_files: self.max_files,
        }
    }
}

pub(crate) struct ScanFilter {
    includes: GlobSet,
    excludes: GlobSet,
    include_all: bool,
}
impl ScanFilter {
    pub(crate) fn new(options: &ScanOptions) -> Result<Self> {
        fn compile(patterns: &[String]) -> Result<GlobSet> {
            ensure!(
                patterns.len() <= 128,
                "At most 128 include/exclude globs are supported"
            );
            let mut builder = GlobSetBuilder::new();
            for pattern in patterns {
                ensure!(
                    !pattern.is_empty()
                        && pattern.len() <= 512
                        && !pattern.starts_with('/')
                        && !pattern.contains(['\\', '\0'])
                        && !pattern.split('/').any(|part| part == ".."),
                    "Globs must be bounded project-relative slash-separated patterns without parent traversal"
                );
                builder.add(
                    GlobBuilder::new(pattern)
                        .literal_separator(true)
                        .backslash_escape(false)
                        .build()
                        .with_context(|| format!("Invalid discovery glob {pattern:?}"))?,
                );
            }
            builder.build().context("Cannot compile discovery globs")
        }
        Ok(Self {
            includes: compile(&options.include_globs)?,
            excludes: compile(&options.exclude_globs)?,
            include_all: options.include_globs.is_empty(),
        })
    }
    pub(crate) fn excluded(&self, path: &Path) -> bool {
        self.excludes.is_match(path)
    }
    pub(crate) fn directory_excluded(&self, path: &Path) -> bool {
        self.excluded(path)
            || self
                .excludes
                .is_match(format!("{}/", path.to_string_lossy()))
    }
    pub(crate) fn candidate(&self, path: &Path) -> bool {
        !self.excluded(path) && (self.include_all || self.includes.is_match(path))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionEvidence {
    pub path: PathBuf,
    pub field: Option<String>,
    pub line: Option<usize>,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectionFact {
    pub name: String,
    pub value: String,
    pub package_path: PathBuf,
    pub evidence: Vec<InspectionEvidence>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectInspection {
    pub project_id: String,
    pub inspected_at: String,
    pub files_inspected: usize,
    pub languages: Vec<InspectionFact>,
    pub package_managers: Vec<InspectionFact>,
    pub runtime_requirements: Vec<InspectionFact>,
    pub lockfiles: Vec<InspectionFact>,
    pub workspaces: Vec<InspectionFact>,
    pub ci_test_commands: Vec<InspectionFact>,
    pub coverage_configurations: Vec<InspectionFact>,
    pub warnings: Vec<ScanWarning>,
    pub limitations: Vec<String>,
}
impl Core {
    pub fn scan_profile(&self, project_id: &str) -> Result<ScanProfile> {
        ensure!(
            self.store()?.project(project_id)?.is_some(),
            "Unknown project"
        );
        let profile = self
            .store()?
            .get_setting(&format!("project.{project_id}.scan_profile"))?
            .map(|value| serde_json::from_str::<ScanProfile>(&value))
            .transpose()
            .context("Stored scan profile is malformed")?
            .unwrap_or_default();
        profile.validate()?;
        Ok(profile)
    }
    pub fn configure_scan_profile(
        &self,
        project_id: &str,
        profile: ScanProfile,
    ) -> Result<ScanProfile> {
        ensure!(
            self.store()?.project(project_id)?.is_some(),
            "Unknown project"
        );
        profile.validate()?;
        self.store()?.set_setting(
            &format!("project.{project_id}.scan_profile"),
            &serde_json::to_string(&profile)?,
        )?;
        Ok(profile)
    }
    pub fn inspection(&self, project_id: &str) -> Result<ProjectInspection> {
        let project = self
            .store()?
            .project(project_id)?
            .context("Unknown project")?;
        inspect_project(&project, &self.scan_profile(project_id)?.options())
    }
}

/// Inspect declarative metadata and filenames only. No runtime availability,
/// configuration evaluation, dependency resolution or runner invocation occurs.
pub fn inspect_project(project: &Project, options: &ScanOptions) -> Result<ProjectInspection> {
    let root = project
        .path
        .canonicalize()
        .context("Cannot resolve project directory")?;
    ensure!(root.is_dir(), "Project path is not a directory");
    let filter = ScanFilter::new(options)?;
    let mut report=ProjectInspection {
        project_id:project.id.clone(),inspected_at:timestamp(),files_inspected:0,
        languages:vec![],package_managers:vec![],runtime_requirements:vec![],lockfiles:vec![],workspaces:vec![],ci_test_commands:vec![],coverage_configurations:vec![],warnings:vec![],
        limitations:vec![
            "Passive metadata only: runtime and package-manager availability/version compatibility have not been checked.".into(),
            "Includes select test-file candidates; metadata inspection remains available to preserve package ownership. Excluded content is not read.".into(),
            "Executable configuration is never loaded. Coverage settings inside JavaScript/PHP configuration may require explicit runner enumeration.".into(),
            "CI hints retain recognized test-command tokens only; arguments, environment values, multiline shell semantics and script bodies are omitted. They are not execution commands.".into(),
            "Symlinks, dependency/build directories and unrelated editor-workspace data are not inspected; workspace declarations are not dependency resolution.".into(),
        ],
    };
    let files = crate::scanner::inventory(&root, options, &mut report.warnings)?;
    report.files_inspected = files.len();
    let packages: BTreeSet<_> = files
        .iter()
        .filter(|path| {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("package.json" | "composer.json")
            )
        })
        .map(|path| parent(path))
        .collect();
    for path in &files {
        if filter.excluded(path) {
            continue;
        }
        let package = packages
            .iter()
            .filter(|package| package.as_path() != Path::new(".") && path.starts_with(package))
            .max_by_key(|package| package.components().count())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let language = match extension {
            "php" | "phpt" => Some("PHP"),
            "ts" | "tsx" | "mts" | "cts" => Some("TypeScript"),
            "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
            "py" => Some("Python"),
            "rs" => Some("Rust"),
            "go" => Some("Go"),
            "rb" => Some("Ruby"),
            _ => None,
        };
        if let Some(language) = language {
            if !report
                .languages
                .iter()
                .any(|fact| fact.name == language && fact.package_path == package)
            {
                report.languages.push(fact(
                    path,
                    &package,
                    language,
                    "Source extension observed",
                    None,
                    None,
                    "Filename evidence; no language server or compiler was started",
                ));
            }
        }
        let manager = match name {
            "package-lock.json" | "npm-shrinkwrap.json" => Some("npm"),
            "yarn.lock" => Some("Yarn"),
            "pnpm-lock.yaml" => Some("pnpm"),
            "bun.lock" | "bun.lockb" => Some("Bun"),
            "composer.lock" => Some("Composer"),
            "Cargo.lock" => Some("Cargo"),
            "poetry.lock" => Some("Poetry"),
            "uv.lock" => Some("uv"),
            "Pipfile.lock" => Some("Pipenv"),
            "Gemfile.lock" => Some("Bundler"),
            "go.sum" => Some("Go modules"),
            _ => None,
        };
        if let Some(manager) = manager {
            report.lockfiles.push(fact(
                path,
                &package,
                manager,
                "Lockfile present",
                None,
                None,
                "Existence only; lockfile contents and installed dependencies were not loaded",
            ));
            report.package_managers.push(fact(
                path,
                &package,
                manager,
                "Lockfile evidence",
                None,
                None,
                "Declared package-manager evidence; executable availability is unknown",
            ));
        }
        if matches!(name, "package.json" | "composer.json") {
            let Some(text) = crate::scanner::read_text(&root, path, &mut report.warnings) else {
                continue;
            };
            let metadata = match serde_json::from_str::<Value>(&text) {
                Ok(value) => value,
                Err(error) => {
                    report.warnings.push(ScanWarning{path:path.clone(),message:format!("Malformed metadata JSON at line {}, column {}; rejected values omitted",error.line(),error.column())});
                    continue;
                }
            };
            if !metadata.is_object() {
                report.warnings.push(ScanWarning {
                    path: path.clone(),
                    message: "Package metadata must be a JSON object".into(),
                });
                continue;
            }
            inspect_manifest(
                path,
                &package,
                &metadata,
                name == "composer.json",
                &mut report,
            );
        }
        if matches!(
            name,
            ".nvmrc" | ".node-version" | ".python-version" | ".ruby-version"
        ) {
            let Some(text) = crate::scanner::read_text(&root, path, &mut report.warnings) else {
                continue;
            };
            let runtime = match name {
                ".nvmrc" | ".node-version" => "Node.js",
                ".python-version" => "Python",
                _ => "Ruby",
            };
            if let Some(value) = text
                .lines()
                .next()
                .map(str::trim)
                .filter(|value| safe_requirement(value))
            {
                report.runtime_requirements.push(fact(
                    path,
                    &package,
                    runtime,
                    value,
                    None,
                    Some(1),
                    "Version-file declaration; installed runtime was not probed",
                ));
            }
        }
        if name == "pnpm-workspace.yaml" || name == "lerna.json" || name == "Cargo.toml" {
            report.workspaces.push(fact(
                path,
                &package,
                name,
                "Workspace/package configuration present",
                None,
                None,
                "Presence does not prove resolved workspace membership",
            ));
        }
        if matches!(
            name,
            ".nycrc"
                | ".nycrc.json"
                | ".nycrc.yaml"
                | ".nycrc.yml"
                | ".coveragerc"
                | "codecov.yml"
                | "codecov.yaml"
                | ".codecov.yml"
                | "lcov.info"
        ) || name.starts_with("nyc.config.")
            || name.starts_with("c8.config.")
        {
            report.coverage_configurations.push(fact(path,&package,name,"Coverage configuration/artifact present",None,None,"Configuration/artifact path only; no coverage percentage or freshness was inferred"));
        }
        if name.starts_with("phpunit") && name.contains(".xml") {
            if let Some(text) = crate::scanner::read_text(&root, path, &mut report.warnings) {
                match roxmltree::Document::parse_with_options(
                    &text,
                    roxmltree::ParsingOptions {
                        allow_dtd: false,
                        nodes_limit: 100000,
                        ..Default::default()
                    },
                ) {
                    Ok(document) => {
                        if document
                            .descendants()
                            .any(|node| node.has_tag_name("coverage"))
                        {
                            report.coverage_configurations.push(fact(path,&package,"PHPUnit coverage","Coverage element declared",Some("coverage"),None,"Declarative XML element only; no instrumentation or report was run"));
                        }
                    }
                    Err(_) => report.warnings.push(ScanWarning {
                        path: path.clone(),
                        message: "Malformed/unsupported PHPUnit XML; no configuration was loaded"
                            .into(),
                    }),
                }
            }
        }
        if is_ci_file(path) {
            if let Some(text) = crate::scanner::read_text(&root, path, &mut report.warnings) {
                for (line, value) in text.lines().enumerate() {
                    if let Some(command) = test_command_hint(value) {
                        report.ci_test_commands.push(fact(path,&package,"CI test command",&command,None,Some(line+1),"Recognized command tokens only; omitted arguments may materially change behavior"));
                    }
                }
            }
        }
    }
    for facts in [
        &mut report.languages,
        &mut report.package_managers,
        &mut report.runtime_requirements,
        &mut report.lockfiles,
        &mut report.workspaces,
        &mut report.ci_test_commands,
        &mut report.coverage_configurations,
    ] {
        facts.sort_by(|left, right| {
            (
                &left.package_path,
                &left.name,
                &left.value,
                &left.evidence[0].path,
                left.evidence[0].line,
            )
                .cmp(&(
                    &right.package_path,
                    &right.name,
                    &right.value,
                    &right.evidence[0].path,
                    right.evidence[0].line,
                ))
        });
        facts.dedup();
    }
    report
        .warnings
        .sort_by(|left, right| (&left.path, &left.message).cmp(&(&right.path, &right.message)));
    report.warnings.dedup();
    Ok(report)
}

fn parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_owned()
}
fn fact(
    path: &Path,
    package: &Path,
    name: &str,
    value: &str,
    field: Option<&str>,
    line: Option<usize>,
    detail: &str,
) -> InspectionFact {
    InspectionFact {
        name: name.into(),
        value: value.into(),
        package_path: package.into(),
        evidence: vec![InspectionEvidence {
            path: path.into(),
            field: field.map(str::to_owned),
            line,
            detail: detail.into(),
        }],
    }
}
fn safe_requirement(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || ".*^~<>=| ,+_/-".contains(character)
        })
}
fn inspect_manifest(
    path: &Path,
    package: &Path,
    value: &Value,
    php: bool,
    report: &mut ProjectInspection,
) {
    let manager = if php {
        "Composer"
    } else {
        "Node package manifest"
    };
    report.package_managers.push(fact(
        path,
        package,
        manager,
        "Manifest present",
        None,
        None,
        "Manifest does not establish executable availability",
    ));
    if !php {
        if let Some(manager) = value
            .get("packageManager")
            .and_then(Value::as_str)
            .filter(|value| {
                value.len() <= 300
                    && value.split_once('@').is_some_and(|(name, version)| {
                        ["npm", "pnpm", "yarn", "bun"].contains(&name) && safe_requirement(version)
                    })
            })
        {
            report.package_managers.push(fact(
                path,
                package,
                "packageManager",
                manager,
                Some("packageManager"),
                None,
                "Pinned package-manager declaration; not executed",
            ));
        }
        if let Some(engines) = value.get("engines").and_then(Value::as_object) {
            for (name, version) in engines {
                if ["node", "npm", "pnpm", "yarn", "bun", "deno"].contains(&name.as_str()) {
                    if let Some(version) = version.as_str().filter(|value| safe_requirement(value))
                    {
                        report.runtime_requirements.push(fact(
                            path,
                            package,
                            name,
                            version,
                            Some(&format!("engines.{name}")),
                            None,
                            "Manifest version requirement; installed version is unknown",
                        ));
                    }
                }
            }
        }
        if let Some(workspaces) = value.get("workspaces").and_then(|value| {
            value
                .as_array()
                .or_else(|| value.get("packages").and_then(Value::as_array))
        }) {
            for workspace in workspaces.iter().filter_map(Value::as_str).filter(|value| {
                value.len() <= 300
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric() || "@_-./*{}!".contains(character)
                    })
            }) {
                report.workspaces.push(fact(
                    path,
                    package,
                    "workspace",
                    workspace,
                    Some("workspaces"),
                    None,
                    "Declared workspace pattern only; not expanded outside the scan root",
                ));
            }
        }
        if value
            .get("jest")
            .and_then(|value| value.get("collectCoverage"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            report.coverage_configurations.push(fact(
                path,
                package,
                "Jest coverage",
                "collectCoverage=true",
                Some("jest.collectCoverage"),
                None,
                "Declarative JSON setting; coverage was not executed",
            ));
        }
        for field in ["nyc", "c8"] {
            if value.get(field).is_some() {
                report.coverage_configurations.push(fact(
                    path,
                    package,
                    field,
                    "Coverage object declared",
                    Some(field),
                    None,
                    "Object existence only; potentially sensitive configuration values omitted",
                ));
            }
        }
    } else {
        if let Some(require) = value.get("require").and_then(Value::as_object) {
            for (name, version) in require {
                if name == "php" || name.starts_with("ext-") {
                    if let Some(version) = version.as_str().filter(|value| safe_requirement(value))
                    {
                        report.runtime_requirements.push(fact(
                            path,
                            package,
                            name,
                            version,
                            Some(&format!("require.{name}")),
                            None,
                            "Composer platform requirement; extensions/runtime were not probed",
                        ));
                    }
                }
            }
        }
        if let Some(version) = value
            .pointer("/config/platform/php")
            .and_then(Value::as_str)
            .filter(|value| safe_requirement(value))
        {
            report.runtime_requirements.push(fact(
                path,
                package,
                "Composer PHP platform",
                version,
                Some("config.platform.php"),
                None,
                "Dependency-resolution override, not proof of the actual PHP runtime",
            ));
        }
    }
}
fn is_ci_file(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with(".github/workflows/")
        && matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        )
        || [
            ".gitlab-ci.yml",
            ".circleci/config.yml",
            "azure-pipelines.yml",
            "Jenkinsfile",
        ]
        .contains(&text.as_ref())
}
fn test_command_hint(line: &str) -> Option<String> {
    let tokens: Vec<_> = line
        .split_whitespace()
        .map(|value| value.trim_matches(['\'', '"', ';', '(', ')', ',']))
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let next = tokens.get(index + 1).copied().unwrap_or_default();
        if ["npm", "pnpm", "yarn", "bun", "composer"].contains(token) {
            if next == "test" {
                return Some(format!("{token} test"));
            }
            if next == "run" {
                if let Some(script) = tokens.get(index + 2).filter(|script| {
                    script.starts_with("test")
                        && script.len() <= 120
                        && script.chars().all(|character| {
                            character.is_ascii_alphanumeric() || "_.:-".contains(character)
                        })
                }) {
                    return Some(format!("{token} run {script}"));
                }
            }
        }
        if *token == "node" && next == "--test" {
            return Some("node --test".into());
        }
        if [
            "phpunit",
            "vendor/bin/phpunit",
            "./vendor/bin/phpunit",
            "pest",
            "vendor/bin/pest",
            "jest",
            "vitest",
            "mocha",
            "cypress",
        ]
        .contains(token)
        {
            return Some((*token).to_owned());
        }
        if *token == "playwright" && next == "test" {
            return Some("playwright test".into());
        }
    }
    None
}
