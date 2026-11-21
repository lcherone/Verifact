use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use verifact_core::{Core, Project, default_data_dir};

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
            "Verifact — shared testing core\n\nUsage: verifact-cli [--data-dir PATH] COMMAND\n\n  projects             List registered projects\n  add PATH             Register a directory and passively scan it\n  scan PROJECT_ID|PATH  Refresh discovery without executing project code\n  project PROJECT_ID   Inspect a registered project\n  configure PROJECT_ID [--name NAME] [--url URL]\n                       Save a project name and HTTP(S) browser URL; omitted values stay unchanged\n  tests PROJECT_ID     Show latest framework and test-file candidates\n  history PROJECT_ID   Show the last 20 scans\n  jobs PROJECT_ID      Show the last 20 jobs\n\nTest-file candidates are not confirmed individual test cases. No test commands are executed by these operations."
        );
        return Ok(());
    }
    let core = Core::open(data_dir)?;
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
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Verifact: {error:#}");
        std::process::exit(1);
    }
}
