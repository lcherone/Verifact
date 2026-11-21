use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const WORKER_MODULES: &[&str] = &[
    "browser-worker.mjs",
    "recorder-capture.mjs",
    "recorder-ir.mjs",
    "recorder-evidence.mjs",
    "recorder-verification.mjs",
];

struct WorkerResources {
    root: PathBuf,
    node: PathBuf,
    node_license: PathBuf,
    node_version: String,
    playwright_version: String,
}

impl WorkerResources {
    fn discover(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        // A host runtime probe must not inherit JavaScript injection hooks.
        let output = Command::new("node")
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .args([
                "-p",
                "JSON.stringify({path:process.execPath,version:process.versions.node})",
            ])
            .output()
            .context("Node.js 22+ is required to package the browser worker")?;
        ensure!(
            output.status.success() && output.stdout.len() <= 32768,
            "Cannot identify installed Node.js runtime"
        );
        let metadata: Value =
            serde_json::from_slice(&output.stdout).context("Invalid Node.js runtime metadata")?;
        let node_version = metadata
            .get("version")
            .and_then(Value::as_str)
            .context("Missing Node.js version")?
            .to_owned();
        ensure!(
            node_version
                .split('.')
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|major| major >= 22),
            "Bundled Node.js must be version 22 or newer"
        );
        let node = PathBuf::from(
            metadata
                .get("path")
                .and_then(Value::as_str)
                .context("Missing Node.js executable path")?,
        )
        .canonicalize()?;
        ensure!(
            node.is_file(),
            "Installed Node.js executable is unavailable"
        );
        let prefix = node
            .parent()
            .and_then(Path::parent)
            .context("Cannot identify Node.js installation prefix")?;
        let node_license = [
            prefix.join("LICENSE"),
            prefix.join("share/doc/node/LICENSE"),
            prefix.join("share/licenses/node/LICENSE"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .context("Node.js redistribution LICENSE is missing from its installation prefix")?
        .canonicalize()?;
        ensure!(
            node_license.starts_with(prefix),
            "Node.js LICENSE resolves outside its installation prefix"
        );
        ensure!(
            fs::metadata(&node_license)?.len() <= 2 * 1024 * 1024,
            "Node.js redistribution LICENSE exceeds packaging limit"
        );
        let license_text = fs::read_to_string(&node_license)?;
        ensure!(
            license_text.contains("Node.js") && license_text.contains("Copyright"),
            "Node.js redistribution LICENSE does not identify Node.js"
        );
        ensure!(
            !fs::symlink_metadata(root.join("node_modules"))?
                .file_type()
                .is_symlink(),
            "Installed node_modules cannot be a symbolic link"
        );
        let mut playwright_version = None;
        for name in ["playwright", "playwright-core"] {
            let package = root.join("node_modules").join(name);
            ensure!(
                !fs::symlink_metadata(&package)?.file_type().is_symlink(),
                "Installed {name} must be a real package directory, not a symbolic link"
            );
            let metadata: Value = serde_json::from_slice(&fs::read(package.join("package.json"))?)?;
            ensure!(
                metadata.get("name").and_then(Value::as_str) == Some(name),
                "Installed {name} identity is incorrect"
            );
            let version = metadata
                .get("version")
                .and_then(Value::as_str)
                .context("Installed Playwright package version is missing")?;
            if let Some(expected) = &playwright_version {
                ensure!(
                    expected == version,
                    "Playwright and playwright-core versions differ"
                );
            } else {
                playwright_version = Some(version.to_owned());
            }
            for license in ["LICENSE", "NOTICE"] {
                ensure!(
                    package.join(license).is_file(),
                    "Installed {name}/{license} redistribution notice is missing"
                );
            }
        }
        for module in WORKER_MODULES {
            ensure!(
                root.join("worker").join(module).is_file(),
                "Missing worker module {module}"
            );
        }
        Ok(Self {
            root,
            node,
            node_license,
            node_version,
            playwright_version: playwright_version.context("Playwright is not installed")?,
        })
    }

    fn copy_to(&self, resources: &Path) -> Result<()> {
        let worker = resources.join("worker");
        fs::create_dir_all(&worker)?;
        for module in WORKER_MODULES {
            let source = self.root.join("worker").join(module);
            ensure!(
                !fs::symlink_metadata(&source)?.file_type().is_symlink(),
                "Worker modules cannot be symbolic links"
            );
            ensure!(
                source.canonicalize()?.starts_with(self.root.join("worker")),
                "Worker module resolves outside the source worker directory"
            );
            fs::copy(source, worker.join(module))?;
        }
        let modules = worker.join("node_modules");
        fs::create_dir_all(&modules)?;
        for package in ["playwright", "playwright-core"] {
            let source = self
                .root
                .join("node_modules")
                .join(package)
                .canonicalize()?;
            copy_package(
                &source,
                &source,
                &modules.join(package),
                &mut CopyBudget::default(),
            )?;
        }
        let bin = resources.join("bin");
        let licenses = resources.join("licenses");
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(&licenses)?;
        fs::copy(&self.node, bin.join("node"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join("node"), fs::Permissions::from_mode(0o755))?;
        }
        fs::copy(&self.node_license, licenses.join("Node-LICENSE"))?;
        fs::write(
            resources.join("worker-runtime.json"),
            serde_json::to_vec_pretty(&json!({
                "node":self.node_version,"playwright":self.playwright_version,
                "browserRequirement":"Replay and generated-test verification require the matching Playwright Chromium installation in the operator browser cache. Browser binaries are not bundled.",
                "installBrowserCommand":["Contents/Resources/bin/node","Contents/Resources/worker/node_modules/playwright/cli.js","install","chromium"]
            }))?,
        )?;
        Ok(())
    }
}

#[derive(Default)]
struct CopyBudget {
    files: usize,
    bytes: u64,
}
fn copy_package(
    root: &Path,
    source: &Path,
    destination: &Path,
    budget: &mut CopyBudget,
) -> Result<()> {
    budget.files += 1;
    ensure!(
        budget.files <= 20000,
        "Installed package exceeds packaging entry limit"
    );
    let metadata = fs::symlink_metadata(source)?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "Package copy refuses symbolic link {}",
        source.display()
    );
    ensure!(
        source.canonicalize()?.starts_with(root),
        "Package copy escaped its installed package root"
    );
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_package(
                root,
                &entry.path(),
                &destination.join(entry.file_name()),
                budget,
            )?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "Package copy refuses a non-regular file"
        );
        budget.bytes = budget.bytes.saturating_add(metadata.len());
        ensure!(
            budget.files <= 20000 && budget.bytes <= 512 * 1024 * 1024,
            "Installed package exceeds packaging copy limits"
        );
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let resources_plan = WorkerResources::discover(Path::new("."))?;
    // Build in isolation. A failed resource copy leaves the last usable app intact.
    fs::create_dir_all("target/bundle")?;
    let staging = tempfile::Builder::new()
        .prefix(".verifact-build-")
        .tempdir_in("target/bundle")?;
    let info = cef::build_util::mac::BundleInfo::new(
        "Verifact",
        "local.verifact.desktop",
        "Verifact",
        "English",
        "0.1.0".parse()?,
    );
    let path = cef::build_util::mac::bundle(
        staging.path(),
        std::path::Path::new("target/debug"),
        "verifact",
        "verifact-helper",
        None,
        info,
    )?;
    let resources = path.join("Contents/Resources");
    std::fs::create_dir_all(&resources)?;
    std::fs::copy(
        "assets/brand/Verifact.icns",
        resources.join("Verifact.icns"),
    )?;
    std::fs::copy(
        "target/debug/verifact-cli",
        path.join("Contents/MacOS/verifact-cli"),
    )?;
    resources_plan.copy_to(&resources)?;
    let manifest = path.join("Contents/Info.plist");
    let mut metadata = plist::Value::from_file(&manifest)?;
    let dictionary = metadata
        .as_dictionary_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid application Info.plist"))?;
    dictionary.insert("CFBundleIconFile".into(), "Verifact.icns".into());
    dictionary.insert("LSMinimumSystemVersion".into(), "13.0".into());
    metadata.to_file_xml(manifest)?;
    // cef-rs currently reuses the main identifier for all helpers. Give each
    // bundle its own identity so Launch Services and native automation can
    // distinguish the UI application from Chromium subprocesses.
    for entry in std::fs::read_dir(path.join("Contents/Frameworks"))? {
        let helper = entry?.path();
        if helper
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            let info_path = helper.join("Contents/Info.plist");
            let mut value = plist::Value::from_file(&info_path)?;
            let dictionary = value
                .as_dictionary_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid helper Info.plist"))?;
            let name = helper
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid helper bundle name"))?;
            let suffix: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
            dictionary.insert(
                "CFBundleIdentifier".into(),
                format!("local.verifact.desktop.{suffix}").into(),
            );
            dictionary.insert(
                "CFBundleDisplayName".into(),
                name.replace("verifact", "Verifact").into(),
            );
            dictionary.insert(
                "CFBundleName".into(),
                name.replace("verifact", "Verifact").into(),
            );
            dictionary.insert("LSMinimumSystemVersion".into(), "13.0".into());
            value.to_file_xml(info_path)?;
        }
    }
    let branded_path = Path::new("target/bundle/Verifact.app");
    let previous = staging.path().join("previous.app");
    match fs::symlink_metadata(branded_path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Refusing to replace an app output that is not a real directory"
            );
            fs::rename(branded_path, &previous)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Cannot inspect previous app bundle"),
    }
    if let Err(error) = fs::rename(&path, branded_path) {
        if previous.exists()
            && let Err(restore_error) = fs::rename(&previous, branded_path)
        {
            let recovery = staging.keep();
            bail!(
                "Cannot install bundle ({error}) or restore previous bundle ({restore_error}); recovery preserved at {}",
                recovery.display()
            );
        }
        return Err(error).context("Cannot install completed app bundle");
    }
    println!("{}", branded_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::Stdio,
        time::{Duration, Instant},
    };

    #[test]
    fn copy_rejects_symbolic_links_without_following_them() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let root = source.path().canonicalize().unwrap();
        fs::write(root.join("safe.txt"), "safe").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap();
        #[cfg(unix)]
        assert!(
            copy_package(
                &root,
                &root,
                &destination.path().join("copy"),
                &mut CopyBudget::default()
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "copies installed runtime/dependencies and starts isolated relocated worker"]
    fn relocated_worker_uses_bundled_node_and_packages() {
        let plan = WorkerResources::discover(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let resources = root.join("Relocated.app/Contents/Resources");
        plan.copy_to(&resources).unwrap();
        #[cfg(unix)]
        let invocation = {
            let alias = root.join("resources-alias");
            std::os::unix::fs::symlink(&resources, &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let invocation = resources.clone();
        let mut child = Command::new(invocation.join("bin/node"))
            .arg(invocation.join("worker/browser-worker.mjs"))
            .arg("--data-dir")
            .arg(root.join("acceptance-data"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(b"{\"id\":1,\"method\":\"recording.list\",\"params\":{}}\n")
            .unwrap();
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Relocated worker exceeded acceptance deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response.get("id"), Some(&json!(1)));
        assert_eq!(response.pointer("/result/recordings"), Some(&json!([])));
        assert!(resources.join("licenses/Node-LICENSE").is_file());
        for package in ["playwright", "playwright-core"] {
            for license in ["LICENSE", "NOTICE"] {
                assert!(
                    resources
                        .join("worker/node_modules")
                        .join(package)
                        .join(license)
                        .is_file()
                );
            }
        }
    }
}
