//! Bounded reads of finalized, checksum-verified execution evidence.
use crate::Core;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
};

impl Core {
    pub fn read_artifact(
        &self,
        run_id: &str,
        requested: &Path,
        offset: u64,
        limit: usize,
    ) -> Result<(Vec<u8>, u64)> {
        let run = self.run(run_id)?.context("Unknown run")?;
        ensure!(
            run.status.is_final(),
            "Evidence is available after execution finalization"
        );
        let artifact = run
            .artifacts
            .iter()
            .find(|artifact| artifact.path == requested)
            .context("Artifact is not part of this execution")?;
        ensure!(
            !requested.is_absolute()
                && requested
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "Invalid artifact path"
        );
        let root = self
            .data_dir()
            .canonicalize()?
            .join("runs")
            .join(run_id)
            .canonicalize()?;
        let file = self.data_dir().join(requested).canonicalize()?;
        ensure!(
            file.starts_with(&root),
            "Artifact escapes execution storage"
        );
        let mut file = std::fs::File::open(file)?;
        let length = file.metadata()?.len();
        ensure!(
            length == artifact.bytes && length <= 256 * 1024 * 1024,
            "Artifact size changed or exceeds read budget"
        );
        let expected = artifact
            .sha256
            .as_ref()
            .context("Artifact has no integrity checksum")?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        ensure!(
            format!("{:x}", hash.finalize()) == *expected,
            "Artifact integrity check failed"
        );
        ensure!(offset <= length, "Artifact offset exceeds size");
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(limit.clamp(1, 65536) as u64)
            .read_to_end(&mut bytes)?;
        Ok((bytes, length))
    }
}
