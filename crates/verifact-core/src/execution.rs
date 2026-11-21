//! Approved local execution. Imported source/configuration remains untrusted code;
//! argv isolation and environment filtering are not a sandbox.
use crate::{
    AdapterCommand, Artifact, Core, ExecutionPlan, NativeTestResult, RunOptions, RunOutput,
    RunRecord, RunStatus, ScanOptions, adapters, scan_project, stable_id, timestamp,
};
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CAPTURE_LIMIT: usize = 2 * 1024 * 1024;
const REPORT_LIMIT: u64 = 8 * 1024 * 1024;
const OUTPUT_PAGE_LIMIT: usize = 64 * 1024;
static EXECUTION_SHUTDOWN: Mutex<bool> = Mutex::new(false);

impl Core {
    /// Stop this process from admitting further runs before desktop shutdown.
    /// Existing queued/running work remains visible for cancellation and draining.
    pub fn begin_shutdown(&self) -> Result<()> {
        *EXECUTION_SHUTDOWN
            .lock()
            .map_err(|_| anyhow::anyhow!("Execution admission lock failed"))? = true;
        Ok(())
    }

    pub fn plan_run(
        &self,
        project_id: &str,
        candidate_ids: &[String],
        options: RunOptions,
    ) -> Result<ExecutionPlan> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        self.build_execution_plan(
            project_id,
            candidate_ids,
            options,
            stable_id(&["run", project_id, &nonce, &std::process::id().to_string()]),
            timestamp(),
        )
    }

    fn build_execution_plan(
        &self,
        project_id: &str,
        candidate_ids: &[String],
        options: RunOptions,
        id: String,
        planned_at: String,
    ) -> Result<ExecutionPlan> {
        ensure!(
            (100..=3_600_000).contains(&options.timeout_ms),
            "Run timeout must be between 100 milliseconds and one hour"
        );
        ensure!(
            (1..=16).contains(&options.max_concurrency),
            "Concurrency must be between 1 and 16"
        );
        ensure!(
            (4096..=16 * 1024 * 1024).contains(&options.max_output_bytes),
            "Output limit must be between 4 KiB and 16 MiB"
        );
        ensure!(
            !options.actor.trim().is_empty()
                && options.actor.len() <= 128
                && !options.actor.chars().any(char::is_control),
            "A short initiating actor is required"
        );
        ensure!(
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid run identity"
        );
        let store = self.store()?;
        let project = store.project(project_id)?.context("Unknown project")?;
        let report = store
            .latest_scan(project_id)?
            .context("Scan the project before planning execution")?;
        let requested: HashSet<&str> = candidate_ids.iter().map(String::as_str).collect();
        ensure!(
            requested.len() == candidate_ids.len(),
            "Repeated candidate identifiers are not allowed"
        );
        let selected: Vec<_> = report
            .candidates
            .iter()
            .filter(|candidate| requested.is_empty() || requested.contains(candidate.id.as_str()))
            .cloned()
            .collect();
        ensure!(!selected.is_empty(), "No test candidates selected");
        ensure!(
            requested.is_empty() || selected.len() == requested.len(),
            "Unknown candidate or candidate belongs to another project"
        );
        let mut commands = Vec::new();
        let artifact_root = self.execution_directory(&id)?;
        for candidate in &selected {
            let framework = report
                .frameworks
                .iter()
                .find(|framework| framework.id == candidate.framework_id)
                .context("Candidate has no owning framework")?;
            commands.push(adapters::plan_command(
                &project,
                framework,
                std::slice::from_ref(candidate),
                &artifact_root.join(commands.len().to_string()),
            )?);
        }
        ensure!(!commands.is_empty(), "No supported execution commands");
        let mut ids: Vec<_> = selected
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect();
        ids.sort();
        let current = scan_project(&project, &ScanOptions::default())?;
        let source = crate::source::capture_source(&project.path);
        Ok(ExecutionPlan {
            id,
            project_id: project.id,
            project_path: project.path,
            candidate_ids: ids,
            commands,
            options,
            planned_at,
            revision: current.revision,
            working_tree: None,
            source_fingerprint: source.fingerprint,
        })
    }

    pub fn start_run(&self, plan: ExecutionPlan, approved: bool) -> Result<RunRecord> {
        ensure!(
            approved,
            "Explicit approval is required: project tests and runner configuration execute local code and may modify external systems"
        );
        // Never accept caller-edited program/argv/cwd or paths from a deserialized plan.
        let expected = self.build_execution_plan(
            &plan.project_id,
            &plan.candidate_ids,
            plan.options.clone(),
            plan.id.clone(),
            plan.planned_at.clone(),
        )?;
        ensure!(
            expected == plan,
            "Execution plan changed or is stale; review a new plan before approval"
        );
        let record = RunRecord {
            id: plan.id.clone(),
            project_id: plan.project_id.clone(),
            revision: plan.revision.clone(),
            working_tree: None,
            plan,
            status: RunStatus::Queued,
            queued_at: timestamp(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            exit_code: None,
            results: vec![],
            artifacts: vec![],
            message: None,
            owner_pid: Some(std::process::id()),
            cancel_requested: false,
            output_truncated: false,
            source_state: None,
        };
        let store = self.store()?;
        store.recover_interrupted_runs()?;
        {
            let closing = EXECUTION_SHUTDOWN
                .lock()
                .map_err(|_| anyhow::anyhow!("Execution admission lock failed"))?;
            ensure!(
                !*closing,
                "Verifact is shutting down; new executions are disabled"
            );
            store.insert_run(&record)?;
        }
        let core = self.clone();
        let id = record.id.clone();
        let handle = thread::Builder::new()
            .name(format!("verifact-run-{}", &id[..8]))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    core.execute_run(&id)
                }));
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(_) => Some("Execution worker panicked".into()),
                };
                if let Some(error) = error {
                    core.fail_execution(&id, &error);
                }
            });
        if let Err(error) = handle {
            self.fail_execution(&record.id, &error.to_string());
            return Err(error.into());
        }
        Ok(record)
    }

    pub fn run(&self, id: &str) -> Result<Option<RunRecord>> {
        let store = self.store()?;
        store.recover_interrupted_runs()?;
        store.run(id)
    }

    pub fn run_history(&self, project_id: &str, limit: usize) -> Result<Vec<RunRecord>> {
        let store = self.store()?;
        ensure!(store.project(project_id)?.is_some(), "Unknown project");
        store.recover_interrupted_runs()?;
        store.run_history(project_id, limit)
    }

    pub fn cancel_run(&self, id: &str) -> Result<RunRecord> {
        let store = self.store()?;
        store.request_run_cancel(id)?;
        store.run(id)?.context("Unknown run")
    }

    pub fn active_runs(&self) -> Result<Vec<RunRecord>> {
        let store = self.store()?;
        store.recover_interrupted_runs()?;
        store.active_runs()
    }

    pub fn run_output(&self, id: &str, offset: u64, limit: usize) -> Result<RunOutput> {
        let record = self.run(id)?.context("Unknown run")?;
        let path = self.execution_directory(&record.id)?.join("output.log");
        let mut text = String::new();
        let mut next_offset = offset;
        match open_managed_read(&path) {
            Ok(mut file) => {
                let length = file.metadata()?.len();
                ensure!(
                    offset <= length,
                    "Output offset is beyond the stored output"
                );
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                file.take(limit.min(OUTPUT_PAGE_LIMIT) as u64)
                    .read_to_end(&mut bytes)?;
                next_offset += bytes.len() as u64;
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                ensure!(offset == 0, "Output has not been created");
            }
            Err(error) => return Err(error),
        }
        Ok(RunOutput {
            run_id: id.into(),
            offset,
            next_offset,
            text,
            complete: record.status.is_final(),
            truncated: record.output_truncated,
        })
    }

    fn execution_directory(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid run identity"
        );
        Ok(fs::canonicalize(self.data_dir())?.join("runs").join(id))
    }

    fn fail_execution(&self, id: &str, message: &str) {
        let result = (|| -> Result<()> {
            let store = self.store()?;
            if let Some(mut record) = store.run(id)?
                && !record.status.is_final()
            {
                record.status = RunStatus::InfrastructureError;
                record.finished_at = Some(timestamp());
                record.message = Some(redact(message));
                store.update_run(&record)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "Verifact could not finalize execution: {}",
                redact(&error.to_string())
            );
        }
    }

    fn execute_run(&self, id: &str) -> Result<()> {
        let store = self.store()?;
        let mut record = store.run(id)?.context("Queued run disappeared")?;
        loop {
            let current = store.run(id)?.context("Queued run disappeared")?;
            if current.status.is_final() {
                return Ok(());
            }
            if current.cancel_requested {
                record.cancel_requested = true;
                record.status = RunStatus::Cancelled;
                record.finished_at = Some(timestamp());
                store.update_run(&record)?;
                return Ok(());
            }
            store.recover_interrupted_runs()?;
            if store.claim_run(id, record.plan.options.max_concurrency)? {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        record.status = RunStatus::Running;
        record.started_at = Some(timestamp());
        store.update_run(&record)?;
        let started = Instant::now();
        let directory = self.execution_directory(id)?;
        create_private_directory(&directory)?;
        let log_path = directory.join("output.log");
        let output = Arc::new(Mutex::new(OutputSink {
            file: private_file(&log_path)?,
            written: 0,
            limit: record.plan.options.max_output_bytes,
            truncated: false,
        }));
        // Provenance is measured only after explicit execution approval. No project hooks/configured helpers.
        let source = crate::source::capture_source(&record.plan.project_path);
        let source_changed = record
            .plan
            .source_fingerprint
            .as_ref()
            .is_some_and(|fingerprint| source.fingerprint.as_ref() != Some(fingerprint));
        record.revision = source.revision.clone().or(record.revision);
        record.working_tree = source.fingerprint.as_ref().map(|fingerprint| {
            format!(
                "{}; content_sha256={fingerprint}",
                match source.dirty {
                    Some(false) => "clean",
                    Some(true) => "dirty",
                    None => "unclassified",
                }
            )
        });
        record.source_state = Some(source);
        store.update_run(&record)?;
        let mut outcome = if source_changed {
            record.message = Some("Source changed or became unmeasurable while this run was queued; review a new execution plan.".into());
            RunStatus::Blocked
        } else {
            RunStatus::Skipped
        };
        for command in record.plan.commands.clone() {
            if source_changed {
                break;
            }
            let current = store.run(id)?.context("Run disappeared")?;
            if current.cancel_requested {
                record.cancel_requested = true;
                outcome = RunStatus::Cancelled;
                break;
            }
            create_private_directory(
                command
                    .report_path
                    .parent()
                    .context("Missing report directory")?,
            )?;
            let deadline = Duration::from_millis(record.plan.options.timeout_ms);
            if started.elapsed() >= deadline {
                outcome = RunStatus::TimedOut;
                break;
            }
            let process_result = execute_command(
                &command,
                output.clone(),
                deadline.saturating_sub(started.elapsed()),
                || Ok(store.run(id)?.is_some_and(|run| run.cancel_requested)),
            );
            // Scrub reports on cancellation, timeout and parser failures as well as success.
            sanitize_report(&command.report_path)?;
            let result = process_result?;
            record.exit_code = result.exit_code;
            if let Some(status) = result.interrupted {
                outcome = status;
                record.cancel_requested = status == RunStatus::Cancelled;
                break;
            }
            if result.capture_truncated && !command.report_path.is_file() {
                outcome = RunStatus::InfrastructureError;
                record.message = Some("Runner output exceeded the structured parser capture limit; result is incomplete.".into());
                break;
            }
            let mut results = adapters::parse_results(
                &command,
                &result.stdout,
                &result.stderr,
                result.exit_code,
            )?;
            for result in &mut results {
                result.candidate_id = command.candidate_ids.first().cloned();
                result.name = redact(&result.name);
                result.native_id = redact(&result.native_id);
                result.message = result.message.as_deref().map(redact);
            }
            let command_status = summarize(&results, result.exit_code);
            outcome = combine(outcome, command_status);
            record.results.extend(results);
            store.update_run(&record)?;
        }
        let sink = output
            .lock()
            .map_err(|_| anyhow::anyhow!("Output writer failed"))?;
        record.output_truncated = sink.truncated;
        drop(sink);
        record.artifacts = collect_artifacts(self.data_dir(), &directory, &record.plan.commands)?;
        record.duration_ms = Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        record.status = outcome;
        record.finished_at = Some(timestamp());
        if record.results.is_empty() && outcome == RunStatus::Passed {
            record.status = RunStatus::Blocked;
            record.message = Some("No native test results were reported; a successful process exit alone does not verify tests.".into());
        }
        store.update_run(&record)?;
        Ok(())
    }
}

fn summarize(results: &[NativeTestResult], exit_code: Option<i32>) -> RunStatus {
    if results
        .iter()
        .any(|result| result.status == RunStatus::InfrastructureError)
    {
        return RunStatus::InfrastructureError;
    }
    if results
        .iter()
        .any(|result| result.status == RunStatus::Failed)
    {
        return RunStatus::Failed;
    }
    if exit_code != Some(0) {
        return RunStatus::InfrastructureError;
    }
    if results.is_empty()
        || results
            .iter()
            .any(|result| result.status == RunStatus::Blocked)
    {
        return RunStatus::Blocked;
    }
    if results
        .iter()
        .all(|result| result.status == RunStatus::Skipped)
    {
        return RunStatus::Skipped;
    }
    if results
        .iter()
        .any(|result| result.status != RunStatus::Passed && result.status != RunStatus::Skipped)
    {
        return RunStatus::InfrastructureError;
    }
    RunStatus::Passed
}
fn combine(left: RunStatus, right: RunStatus) -> RunStatus {
    for status in [
        RunStatus::InfrastructureError,
        RunStatus::Failed,
        RunStatus::Blocked,
    ] {
        if left == status || right == status {
            return status;
        }
    }
    if left == RunStatus::Passed || right == RunStatus::Passed {
        RunStatus::Passed
    } else {
        RunStatus::Skipped
    }
}

struct CommandResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    interrupted: Option<RunStatus>,
    capture_truncated: bool,
}
struct ChildGuard {
    child: Child,
    reaped: bool,
}
impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
    fn exited(&mut self) -> Result<bool> {
        #[cfg(unix)]
        {
            // WNOWAIT observes without reaping, preventing PID reuse before group cleanup.
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let information = unsafe { information.assume_init() };
            Ok(unsafe { information.si_pid() } != 0)
        }
        #[cfg(not(unix))]
        {
            Ok(self.child.try_wait()?.is_some())
        }
    }
    fn terminate(&mut self) -> Option<std::process::ExitStatus> {
        if self.reaped {
            return None;
        }
        #[cfg(unix)]
        {
            // SAFETY: this child was spawned into a new process group. A negative
            // validated PID targets that group, never the application process group.
            if let Ok(pid) = i32::try_from(self.child.id()) {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        let status = self.child.wait().ok();
        self.reaped = true;
        status
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn configure_environment(command: &mut Command) {
    command.env_clear();
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
            command.env(key, value);
        }
    }
    command
        .env("CI", "1")
        .env("NO_COLOR", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
}
fn execute_command(
    command: &AdapterCommand,
    sink: Arc<Mutex<OutputSink>>,
    timeout: Duration,
    cancelled: impl Fn() -> Result<bool>,
) -> Result<CommandResult> {
    #[cfg(not(unix))]
    bail!("Process-group-safe execution is currently supported on Unix only");
    let mut process = Command::new(&command.program);
    configure_environment(&mut process);
    process
        .envs(&command.environment)
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = ChildGuard::new(
        process
            .spawn()
            .with_context(|| format!("Could not start {}", command.program.display()))?,
    );
    let stdout = child.child.stdout.take().context("Missing stdout pipe")?;
    let stderr = child.child.stderr.take().context("Missing stderr pipe")?;
    let output = sink.clone();
    let stdout_reader = thread::spawn(move || read_stream(stdout, output, "stdout"));
    let stderr_reader = thread::spawn(move || read_stream(stderr, sink, "stderr"));
    let started = Instant::now();
    let mut interrupted = None;
    let exit_code = loop {
        if fs::symlink_metadata(&command.report_path).is_ok_and(|metadata| {
            metadata.len() > REPORT_LIMIT || metadata.file_type().is_symlink()
        }) {
            interrupted = Some(RunStatus::InfrastructureError);
            child.terminate();
            break None;
        }
        if cancelled()? {
            interrupted = Some(RunStatus::Cancelled);
            child.terminate();
            break None;
        }
        if started.elapsed() >= timeout {
            interrupted = Some(RunStatus::TimedOut);
            child.terminate();
            break None;
        }
        if child.exited()? {
            break child.terminate().and_then(|status| status.code());
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Clean up child descendants before joining pipe readers (descendants can inherit pipes).
    child.terminate();
    let (stdout, out_truncated) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Stdout reader panicked"))??;
    let (stderr, err_truncated) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Stderr reader panicked"))??;
    Ok(CommandResult {
        stdout,
        stderr,
        exit_code,
        interrupted,
        capture_truncated: out_truncated || err_truncated,
    })
}

struct OutputSink {
    file: File,
    written: u64,
    limit: u64,
    truncated: bool,
}
impl OutputSink {
    fn append(&mut self, text: &str, source: &str) -> Result<()> {
        let line = format!("[{source}] {}", redact(text));
        let remaining = self.limit.saturating_sub(self.written) as usize;
        let mut length = remaining.min(line.len());
        while length > 0 && !line.is_char_boundary(length) {
            length -= 1;
        }
        self.file.write_all(&line.as_bytes()[..length])?;
        self.file.flush()?;
        self.written += length as u64;
        self.truncated |= length < line.len();
        Ok(())
    }
}
fn read_stream(
    mut stream: impl Read,
    sink: Arc<Mutex<OutputSink>>,
    source: &str,
) -> Result<(String, bool)> {
    let mut buffer = [0u8; 4096];
    let mut capture = Vec::new();
    let mut pending = Vec::new();
    let mut truncated = false;
    let mut oversized = false;
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(CAPTURE_LIMIT.saturating_sub(capture.len()));
        capture.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                let text = if oversized {
                    "[oversized output line withheld]\n".into()
                } else {
                    String::from_utf8_lossy(&pending).into_owned() + "\n"
                };
                sink.lock()
                    .map_err(|_| anyhow::anyhow!("Output writer failed"))?
                    .append(&text, source)?;
                pending.clear();
                oversized = false;
            } else if pending.len() < 64 * 1024 && !oversized {
                pending.push(*byte);
            } else {
                pending.clear();
                oversized = true;
            }
        }
    }
    if oversized || !pending.is_empty() {
        let text = if oversized {
            "[oversized output line withheld]".into()
        } else {
            String::from_utf8_lossy(&pending).into_owned()
        };
        sink.lock()
            .map_err(|_| anyhow::anyhow!("Output writer failed"))?
            .append(&text, source)?;
    }
    Ok((String::from_utf8_lossy(&capture).into_owned(), truncated))
}

pub(crate) fn redact(value: &str) -> String {
    static RULES: OnceLock<std::result::Result<Vec<Regex>, regex::Error>> = OnceLock::new();
    let rules = RULES.get_or_init(|| [
        r"(?i)(https?://)[^\s/@]*@",
        r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+",
        r#"(?i)(["'](?:authorization|set-cookie|cookie)["']\s*:\s*["'])[^"'\r\n]*"#,
        r"(?im)^((?:authorization|set-cookie|cookie)\s*:\s*)[^\r\n]*",
        r#"(?i)((?:password|passwd|secret|access_token|refresh_token|api[_-]?key|authorization|set-cookie|cookie|token)[\"']?\s*[:=]\s*[\"']?)[^\s\"',;<>]+"#,
    ].iter().map(|rule| Regex::new(rule)).collect());
    let Ok(rules) = rules else {
        return "[Output withheld: redaction unavailable]".into();
    };
    rules.iter().fold(value.to_owned(), |value, rule| {
        rule.replace_all(&value, "${1}[REDACTED]").into_owned()
    })
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        fs::canonicalize(path)? == path,
        "Artifact directory cannot traverse symbolic links"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Artifact directory cannot be a symbolic link"
    );
    Ok(())
}
fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}
fn open_managed_read(path: &Path) -> Result<File> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "Artifact must be a regular file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}
fn sanitize_report(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let file = open_managed_read(path)?;
    if file.metadata()?.len() > REPORT_LIMIT {
        drop(file);
        let mut options = OpenOptions::new();
        options.write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        options
            .open(path)?
            .write_all(b"[Oversized native report withheld]")?;
        bail!("Native report exceeds the 8 MiB limit");
    }
    let mut bytes = Vec::new();
    file.take(REPORT_LIMIT + 1).read_to_end(&mut bytes)?;
    let text = redact(&String::from_utf8_lossy(&bytes));
    // File is opened without following links; reports are project-generated untrusted data.
    let mut options = OpenOptions::new();
    options.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
fn collect_artifacts(
    data_dir: &Path,
    directory: &Path,
    commands: &[AdapterCommand],
) -> Result<Vec<Artifact>> {
    let base = fs::canonicalize(data_dir)?;
    let mut paths = BTreeSet::new();
    paths.insert(directory.join("output.log"));
    for command in commands {
        if command.report_path.exists() {
            paths.insert(command.report_path.clone());
        }
    }
    let mut artifacts = Vec::new();
    for path in paths {
        let mut file = open_managed_read(&path)?;
        let size = file.metadata()?.len();
        ensure!(
            size <= REPORT_LIMIT.max(16 * 1024 * 1024),
            "Artifact exceeds retention size limit"
        );
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        artifacts.push(Artifact {
            kind: if path.file_name().is_some_and(|name| name == "output.log") {
                "output"
            } else {
                "native_report"
            }
            .into(),
            path: path.strip_prefix(&base)?.to_owned(),
            bytes: size,
            sha256: Some(format!("{:x}", hash.finalize())),
        });
    }
    Ok(artifacts)
}

pub(crate) fn host_git_output(directory: &Path, arguments: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    configure_environment(&mut command);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = ChildGuard::new(command.spawn().ok()?);
    let pipe = child.child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.take(256 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let start = Instant::now();
    let success = loop {
        if start.elapsed() > Duration::from_secs(3) {
            break false;
        }
        match child.exited() {
            Ok(true) => break child.terminate().is_some_and(|status| status.success()),
            Ok(false) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break false,
        }
    };
    child.terminate();
    let bytes = reader.join().ok()?;
    if !success || bytes.len() >= 256 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn redaction_handles_credentials_bearer_headers_and_split_stream_chunks() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("redacted.log");
        let output = Arc::new(Mutex::new(OutputSink {
            file: private_file(&path)?,
            written: 0,
            limit: 100_000,
            truncated: false,
        }));
        let value = format!(
            "{}🔒 password=do-not-save\nAuthorization: Bearer private-token\nAuthorization: Basic encoded-credential\nCookie: first=first-private; second=second-private\nhttps://person:private-pass@example.test/path\nhttps://:empty-user-secret@example.test/\n{{\"authorization\":\"Basic json-credential\"}}\n",
            "x".repeat(4094)
        );
        read_stream(Cursor::new(value), output.clone(), "stdout")?;
        drop(output);
        let stored = fs::read_to_string(path)?;
        for secret in [
            "do-not-save",
            "private-token",
            "private-pass",
            "encoded-credential",
            "first-private",
            "second-private",
            "json-credential",
            "empty-user-secret",
        ] {
            assert!(!stored.contains(secret), "Leaked {secret}");
        }
        assert!(stored.contains("REDACTED"));
        assert!(stored.contains('🔒'));
        Ok(())
    }

    #[test]
    fn empty_or_skipped_cases_do_not_become_a_pass() {
        assert_eq!(summarize(&[], Some(0)), RunStatus::Blocked);
        let skipped = NativeTestResult {
            candidate_id: None,
            native_id: "skip".into(),
            name: "skipped".into(),
            status: RunStatus::Skipped,
            duration_ms: None,
            assertions: None,
            message: None,
        };
        assert_eq!(summarize(&[skipped], Some(0)), RunStatus::Skipped);
    }
}
