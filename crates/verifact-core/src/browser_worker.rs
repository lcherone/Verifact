//! Process boundary for the Playwright worker. Native views submit requests on
//! background threads; this module contains no GPUI or browser-host code.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

const MAX_MESSAGE_BYTES: u64 = 8 * 1024 * 1024;

pub struct BrowserWorker {
    process: Arc<Mutex<Child>>,
    input: mpsc::SyncSender<Vec<u8>>,
    writes: mpsc::Receiver<Result<(), String>>,
    responses: mpsc::Receiver<Result<Value, String>>,
    notifications: Arc<Mutex<VecDeque<Value>>>,
    next_id: u64,
}

#[derive(Clone)]
pub struct BrowserWorkerControl {
    process: Arc<Mutex<Child>>,
}

impl BrowserWorkerControl {
    pub fn cancel_replay(&self) -> Result<()> {
        let mut process = self
            .process
            .lock()
            .map_err(|_| anyhow::anyhow!("Browser worker control unavailable"))?;
        ensure!(process.try_wait()?.is_none(), "Browser worker has stopped");
        #[cfg(unix)]
        {
            // Keep the unreaped Child locked until signaling, so its identity
            // cannot be reused between the liveness check and interruption.
            ensure!(
                unsafe { libc::kill(process.id() as i32, libc::SIGUSR1) } == 0,
                "Cannot interrupt browser replay"
            );
            Ok(())
        }
        #[cfg(not(unix))]
        bail!("Replay interruption is unavailable on this platform")
    }

    pub fn shutdown(&self) {
        self.stop();
    }

    fn stop(&self) {
        let Ok(mut process) = self.process.lock() else {
            return;
        };
        if process.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(process.id() as i32, libc::SIGTERM);
        }
        for _ in 0..500 {
            if process.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = process.kill();
        let _ = process.wait();
    }
}

impl BrowserWorker {
    pub fn spawn(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let data_dir = data_dir.canonicalize()?;
        let script = worker_script()?;
        let mut process = Command::new(node_executable()?)
            .arg(&script)
            .arg("--data-dir")
            .arg(&data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context(
                "Cannot start browser worker; install Node.js and the Playwright dependencies",
            )?;
        let mut input = process
            .stdin
            .take()
            .context("Browser worker stdin unavailable")?;
        let output = process
            .stdout
            .take()
            .context("Browser worker stdout unavailable")?;
        let (write_sender, write_receiver) = mpsc::sync_channel::<Vec<u8>>(1);
        let (written, writes) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok(bytes) = write_receiver.recv() {
                let result = input
                    .write_all(&bytes)
                    .and_then(|_| input.write_all(b"\n"))
                    .and_then(|_| input.flush())
                    .map_err(|error| error.to_string());
                let failed = result.is_err();
                if written.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let notifications = Arc::new(Mutex::new(VecDeque::new()));
        let events = notifications.clone();
        let (sender, responses) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut bytes = Vec::new();
                let read = (&mut reader)
                    .take(MAX_MESSAGE_BYTES + 1)
                    .read_until(b'\n', &mut bytes);
                let value = match read {
                    Ok(0) => break,
                    Ok(_) if bytes.len() as u64 > MAX_MESSAGE_BYTES => {
                        let _ = sender.send(Err("Browser worker message exceeds limit".into()));
                        break;
                    }
                    Ok(_) => serde_json::from_slice::<Value>(&bytes)
                        .map_err(|error| format!("Invalid browser worker response: {error}")),
                    Err(error) => Err(format!("Browser worker read failed: {error}")),
                };
                if let Ok(value) = &value
                    && value.get("id").is_none()
                {
                    if let Ok(mut queue) = events.lock() {
                        if queue.len() == 64 {
                            queue.pop_front();
                        }
                        queue.push_back(value.clone());
                    }
                    continue;
                }
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            process: Arc::new(Mutex::new(process)),
            input: write_sender,
            writes,
            responses,
            notifications,
            next_id: 1,
        })
    }

    pub fn control(&self) -> BrowserWorkerControl {
        BrowserWorkerControl {
            process: self.process.clone(),
        }
    }

    /// Requests are serialized by ownership. On timeout the worker is stopped;
    /// callers must create a fresh worker rather than consume a late response.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        ensure!(
            self.process
                .lock()
                .map_err(|_| anyhow::anyhow!("Browser worker control unavailable"))?
                .try_wait()?
                .is_none(),
            "Browser worker is not running"
        );
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("Worker request ID exhausted")?;
        let request = json!({"id":id,"method":method,"params":params});
        let bytes = serde_json::to_vec(&request)?;
        ensure!(
            bytes.len() as u64 <= MAX_MESSAGE_BYTES,
            "Browser worker request exceeds limit"
        );
        if let Err(error) = self.input.try_send(bytes) {
            self.control().stop();
            bail!("Browser worker write queue unavailable: {error}");
        }
        match self.writes.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            error => {
                self.control().stop();
                bail!("Browser worker input did not accept the request: {error:?}");
            }
        }
        let response = match self.responses.recv_timeout(Duration::from_secs(120)) {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.control().stop();
                bail!("{error}");
            }
            Err(error) => {
                self.control().stop();
                bail!("Browser worker did not respond: {error}");
            }
        };
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            self.control().stop();
            bail!("Mismatched browser worker response");
        }
        if let Some(error) = response.get("error") {
            bail!(
                "{}",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Browser worker request failed")
            );
        }
        if let Some(result) = response.get("result") {
            return Ok(result.clone());
        }
        self.control().stop();
        bail!("Browser worker returned no result")
    }

    pub fn notifications(&self) -> Vec<Value> {
        self.notifications
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }
}

impl Drop for BrowserWorker {
    fn drop(&mut self) {
        self.control().stop();
    }
}

fn node_executable() -> Result<std::ffi::OsString> {
    if let Some(executable) = std::env::var_os("VERIFACT_NODE") {
        return Ok(executable);
    }
    if let Some(parent) = std::env::current_exe()?.parent() {
        let bundled = parent.join("../Resources/bin/node");
        if bundled.is_file() {
            return Ok(bundled.into_os_string());
        }
    }
    Ok("node".into())
}

fn worker_script() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("VERIFACT_BROWSER_WORKER") {
        let path = PathBuf::from(path);
        ensure!(path.is_file(), "Configured browser worker is missing");
        return Ok(path);
    }
    if let Some(parent) = std::env::current_exe()?.parent() {
        let bundled = parent.join("../Resources/worker/browser-worker.mjs");
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../worker/browser-worker.mjs");
    ensure!(development.is_file(), "Browser worker is not installed");
    Ok(development)
}

impl crate::Core {
    pub fn recording_data_dir(&self, project_id: &str) -> Result<PathBuf> {
        ensure!(
            project_id.len() == 64 && project_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid project identity"
        );
        ensure!(
            self.store()?.project(project_id)?.is_some(),
            "Unknown project"
        );
        let directory = self.data_dir().join("browser-recordings").join(project_id);
        std::fs::create_dir_all(&directory)?;
        Ok(directory.canonicalize()?)
    }
}
