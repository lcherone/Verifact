//! Bounded, read-only source identity. Git-visible files are fingerprinted without
//! invoking project hooks, filters, runners or configured filesystem monitors.
use crate::{Core, SourceState, execution::host_git_output, timestamp};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Component, Path},
};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

impl Core {
    pub fn source_state(&self, project_id: &str) -> Result<SourceState> {
        let project = self
            .store()?
            .project(project_id)?
            .context("Unknown project")?;
        Ok(capture_source(&project.path))
    }
}

pub(crate) fn capture_source(root: &Path) -> SourceState {
    let mut state = SourceState {
        captured_at: timestamp(),
        limitations: vec!["Fingerprint covers Git-visible tracked and untracked source, excluding ignored files, external services, runtime environment and dependencies not tracked by Git. It is a local observation, not an atomic filesystem snapshot or independent attestation.".into()],
        ..SourceState::default()
    };
    match fingerprint(root, &mut state) {
        Ok(fingerprint) => {
            state.fingerprint = Some(fingerprint);
            state.complete = true;
        }
        Err(error) => state
            .limitations
            .push(format!("Source identity unavailable: {error:#}")),
    }
    state
}

fn fingerprint(root: &Path, state: &mut SourceState) -> Result<String> {
    let revision = host_git_output(root, &["rev-parse", "--verify", "HEAD"])
        .context("Git HEAD is unavailable")?
        .trim()
        .to_owned();
    anyhow::ensure!(
        (revision.len() == 40 || revision.len() == 64)
            && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid Git revision"
    );
    state.revision = Some(revision.clone());
    let listing_args = [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ];
    let listing = host_git_output(root, &listing_args)
        .context("Git file listing is unavailable, timed out or exceeded 256 KiB")?;
    // Git status/diff can invoke clean/process filters from local configuration.
    // Content identity does not require those commands or a clean/dirty label.
    state.limitations.push("Clean/dirty classification is unavailable; Git-visible content identity is measured directly without running configured filters.".into());
    let files: BTreeSet<&str> = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();
    anyhow::ensure!(
        files.len() <= MAX_FILES,
        "Source identity exceeds the {MAX_FILES}-file budget"
    );
    let mut digest = Sha256::new();
    digest.update(b"Verifact source identity v1\0");
    digest.update(revision.as_bytes());
    for relative in files {
        let path = Path::new(relative);
        anyhow::ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "Git source contains an unsafe path"
        );
        let path = root.join(path);
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                digest.update(b"deleted\0");
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Symlinks, submodules and special source files are not fingerprinted"
        );
        anyhow::ensure!(
            fs::canonicalize(&path)?.starts_with(root),
            "Source identity cannot follow a directory link outside the project"
        );
        anyhow::ensure!(
            state.bytes.saturating_add(metadata.len()) <= MAX_BYTES,
            "Source identity exceeds the 64 MiB byte budget"
        );
        digest.update(b"file\0");
        digest.update(metadata.len().to_be_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update((metadata.permissions().mode() & 0o111).to_be_bytes());
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&path)?;
        let mut buffer = [0u8; 16_384];
        let mut bytes = 0u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            anyhow::ensure!(
                state.bytes.saturating_add(bytes) <= MAX_BYTES,
                "Source grew beyond the byte budget"
            );
            digest.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        anyhow::ensure!(
            metadata.len() == bytes
                && after.len() == bytes
                && metadata.modified().ok() == after.modified().ok(),
            "Source changed while being fingerprinted"
        );
        state.bytes += bytes;
        state.files += 1;
    }
    anyhow::ensure!(
        host_git_output(root, &listing_args).as_deref() == Some(listing.as_str()),
        "Source file inventory changed while being fingerprinted"
    );
    anyhow::ensure!(
        host_git_output(root, &["rev-parse", "--verify", "HEAD"])
            .is_some_and(|value| value.trim() == revision),
        "Git HEAD changed while being fingerprinted"
    );
    Ok(format!("{:x}", digest.finalize()))
}
