//! Orderly process shutdown for clients that own asynchronous executions.
use crate::Core;
use anyhow::Result;
use std::time::Duration;

impl Core {
    /// Close admission before checking active work, then cancel and wait only
    /// for this process's runs. Other CLI/MCP/native processes retain ownership.
    /// Call on a background thread when a desktop window is closing.
    pub fn shutdown_owned_runs(&self) -> Result<()> {
        self.begin_shutdown()?;
        loop {
            let owned: Vec<_> = self
                .active_runs()?
                .into_iter()
                .filter(|run| run.owner_pid == Some(std::process::id()))
                .collect();
            if owned.is_empty() {
                return Ok(());
            }
            for run in owned {
                if !run.cancel_requested {
                    self.cancel_run(&run.id)?;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
