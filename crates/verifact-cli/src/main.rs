use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use verifact_core::{Core, Project, default_data_dir};
mod mcp;

fn execute(core: &Core, id: &str, args: &[String]) -> Result<serde_json::Value> {
    let approved = args.iter().any(|value| value == "--approve");
    let candidates: Vec<_> = args
        .iter()
        .filter(|value| value.as_str() != "--approve")
        .cloned()
        .collect();
    let plan = core.plan_run(id, &candidates, verifact_core::RunOptions::default())?;
    if !approved {
        return Ok(
            serde_json::json!({"requires_approval":true,"plan":plan,"message":"Review these commands and use --approve to execute project code."}),
        );
    }
    let mut run = core.start_run(plan, true)?;
    let mut offset = 0;
    loop {
        let output = core.run_output(&run.id, offset, 16384)?;
        if !output.text.is_empty() {
            eprint!("{}", output.text);
        }
        offset = output.next_offset;
        if run.status.is_final() {
            if output.text.is_empty() {
                break;
            }
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        run = core.run(&run.id)?.context("Execution record disappeared")?;
    }
    let passed = run.status == verifact_core::RunStatus::Passed;
    println!("{}", serde_json::to_string_pretty(&run)?);
    if !passed {
        bail!("Execution finished with status {}", run.status.as_str());
    }
    // The final record was streamed above, preserving a single JSON document.
    Ok(serde_json::Value::Null)
}

fn recording(
    core: &Core,
    command: &str,
    project_id: &str,
    args: &[String],
) -> Result<serde_json::Value> {
    let mut worker = verifact_core::BrowserWorker::spawn(&core.recording_data_dir(project_id)?)?;
    let method = match command {
        "recordings" => "recording.list",
        "recording" => "recording.get",
        "generate" => "recording.generate_test",
        "replay" => "recording.replay",
        "verify-generated" => "recording.verify_generated_test",
        _ => bail!("Unknown recording operation"),
    };
    let mut params = serde_json::json!({"projectId":project_id});
    if command != "recordings" {
        let id = args.first().context("Recording ID required")?;
        params["id"] = serde_json::json!(id);
        if matches!(command, "replay" | "verify-generated") {
            anyhow::ensure!(
                args.len() == 2 && args[1] == "--approve",
                "Review the recording and pass --approve to execute its actions in a fresh browser"
            );
        } else {
            anyhow::ensure!(args.len() == 1, "Unexpected recording arguments");
        }
    } else {
        anyhow::ensure!(args.is_empty(), "Unexpected recording arguments");
    }
    worker.request(method, params)
}

fn project(core: &Core, identity: &str) -> Result<Project> {
    core.store()?
        .project(identity)?
        .with_context(|| format!("Unknown project: {identity}"))
}

fn configure(core: &Core, id: &str, options: &[String]) -> Result<serde_json::Value> {
    let mut name = None;
    let mut url = None;
    let mut options = options.iter();
    while let Some(option) = options.next() {
        let target = match option.as_str() {
            "--name" => &mut name,
            "--url" => &mut url,
            _ => bail!("Unknown configure option: {option}"),
        };
        if target.is_some() {
            bail!("Repeated configure option: {option}");
        }
        *target = Some(
            options
                .next()
                .with_context(|| format!("Missing value for {option}"))?
                .as_str(),
        );
    }
    let project = core.configure_project(id, name, url)?;
    Ok(serde_json::json!({"project": project, "base_url": core.project_base_url(id)?}))
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = if args.first().is_some_and(|arg| arg == "--data-dir") {
        if args.len() < 3 {
            bail!("Use --data-dir PATH before the command");
        }
        let directory = PathBuf::from(args.remove(1));
        args.remove(0);
        directory
    } else {
        default_data_dir()?
    };
    let command = args.first().map(String::as_str).unwrap_or("help");
    if matches!(command, "help" | "--help" | "-h") {
        println!(
            "Verifact — shared testing core\n\nUsage: verifact-cli [--data-dir PATH] COMMAND\n\n  projects             List registered projects\n  add PATH             Register a directory and passively scan it\n  scan PROJECT_ID|PATH  Refresh discovery without executing project code\n  project PROJECT_ID   Inspect a registered project\n  configure PROJECT_ID [--name NAME] [--url URL]\n                       Save a project name and HTTP(S) browser URL; omitted values stay unchanged\n  tests PROJECT_ID     Show latest framework and test-file candidates\n  history PROJECT_ID   Show the last 20 scans\n  jobs PROJECT_ID      Show the last 20 jobs\n  run PROJECT_ID [TEST_ID ...] [--approve]\n                       Review a run; execute only with --approve\n  runs PROJECT_ID      Show the last 20 executions\n  result RUN_ID        Inspect a real execution\n  cancel RUN_ID        Request execution cancellation\n  recordings PROJECT_ID  List saved browser journeys\n  recording PROJECT_ID RECORDING_ID  Inspect a journey\n  generate PROJECT_ID RECORDING_ID   Preview Playwright source\n  replay PROJECT_ID RECORDING_ID --approve\n  verify-generated PROJECT_ID RECORDING_ID --approve\n  mcp [--allow-execution PROJECT_ID]\n                       Serve local stdio MCP tools\n\nTest-file candidates are not confirmed individual test cases. No test commands are executed by discovery operations. Use run --approve to authorize execution."
        );
        return Ok(());
    }
    let core = Core::open(data_dir)?;
    if command == "mcp" {
        return mcp::serve(core, &args[1..]);
    }
    let value = match (command, args.get(1)) {
        ("projects", None) if args.len() == 1 => serde_json::to_value(core.store()?.projects()?)?,
        ("add", Some(path)) if args.len() == 2 => {
            let project = core.add_project(path)?;
            let report = core.scan(&project)?;
            serde_json::json!({"project": project, "scan": report})
        }
        ("scan", Some(identity)) if args.len() == 2 => {
            let project = if Path::new(identity).is_dir() {
                core.add_project(identity)?
            } else {
                project(&core, identity)?
            };
            serde_json::to_value(core.scan(&project)?)?
        }
        ("project", Some(id)) if args.len() == 2 => serde_json::to_value(project(&core, id)?)?,
        ("configure", Some(id)) => configure(&core, id, &args[2..])?,
        ("run", Some(id)) => execute(&core, id, &args[2..])?,
        ("recordings" | "recording" | "generate" | "replay" | "verify-generated", Some(id)) => {
            recording(&core, command, id, &args[2..])?
        }
        ("runs", Some(id)) if args.len() == 2 => serde_json::to_value(core.run_history(id, 20)?)?,
        ("result", Some(id)) if args.len() == 2 => serde_json::to_value(core.run(id)?)?,
        ("cancel", Some(id)) if args.len() == 2 => serde_json::to_value(core.cancel_run(id)?)?,
        ("tests", Some(id)) if args.len() == 2 => {
            project(&core, id)?;
            serde_json::to_value(core.store()?.latest_scan(id)?)?
        }
        ("history", Some(id)) if args.len() == 2 => {
            project(&core, id)?;
            serde_json::to_value(core.store()?.scan_history(id, 20)?)?
        }
        ("jobs", Some(id)) if args.len() == 2 => {
            project(&core, id)?;
            serde_json::to_value(core.store()?.jobs(id, 20)?)?
        }
        _ => bail!("Unknown command or incorrect arguments. Run verifact-cli --help."),
    };
    if !value.is_null() {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Verifact: {error:#}");
        std::process::exit(1);
    }
}
