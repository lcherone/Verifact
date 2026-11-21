//! Local stdio MCP transport. Execution authority is supplied by the operator
//! when starting this process, never by a tool argument from an external agent.
use base64::Engine as _;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use verifact_core::{Core, RunOptions, RunRecord};

const MAX_REQUEST: u64 = 1024 * 1024;

pub fn serve(core: Core, options: &[String]) -> Result<()> {
    let mut allowed_projects = Vec::new();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        ensure!(option == "--allow-execution", "Unknown MCP option");
        let id = options
            .next()
            .context("--allow-execution requires a project ID")?;
        ensure!(
            core.store()?.project(id)?.is_some(),
            "Unknown execution project"
        );
        allowed_projects.push(id.clone());
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut initialized = false;
    loop {
        let mut bytes = Vec::new();
        let count = (&mut input)
            .take(MAX_REQUEST + 1)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            break;
        }
        if count as u64 > MAX_REQUEST {
            send(
                &mut output,
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Request exceeds limit"}}),
            )?;
            break;
        }
        let request: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                send(
                    &mut output,
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON"}}),
                )?;
                continue;
            }
        };
        let Some(id) = request.get("id").cloned() else {
            // Notifications never execute tools and never receive a response.
            continue;
        };
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            send(
                &mut output,
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Request ID must be a string or integer"}}),
            )?;
            continue;
        }
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            Err((-32600, "Expected JSON-RPC 2.0".to_owned()))
        } else if method == "initialize" {
            if initialized {
                Err((-32600, "Already initialized".to_owned()))
            } else {
                initialized = true;
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let protocol = match requested {
                    "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25" => requested,
                    _ => "2025-11-25",
                };
                Ok(
                    json!({"protocolVersion":protocol,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"verifact","version":env!("CARGO_PKG_VERSION")},"instructions":"Discovery is passive. Test execution is available only for operator-authorized projects. File candidates are not confirmed individual test cases. Results describe the recorded execution, not blanket feature correctness."}),
                )
            }
        } else if !initialized {
            Err((-32000, "Initialize the server first".to_owned()))
        } else {
            match method {
                "ping" => Ok(json!({})),
                "tools/list" => Ok(json!({"tools":tool_definitions()})),
                "tools/call" => match call(&core, &allowed_projects, &params) {
                    Ok(value) => {
                        let structured = if value.is_object() {
                            value
                        } else {
                            json!({"items":value})
                        };
                        Ok(
                            json!({"content":[{"type":"text","text":structured.to_string()}],"structuredContent":structured,"isError":false}),
                        )
                    }
                    Err(error) => Ok(
                        json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}),
                    ),
                },
                _ => Err((-32601, "Method not found".to_owned())),
            }
        };
        let response = match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        send(&mut output, response)?;
    }
    core.shutdown_owned_runs()?;
    Ok(())
}

fn send(output: &mut impl Write, value: Value) -> Result<()> {
    serde_json::to_writer(&mut *output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Missing string field {field}"))
}

fn call(core: &Core, allowed: &[String], params: &Value) -> Result<Value> {
    let name = text(params, "name")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    ensure!(arguments.is_object(), "Tool arguments must be an object");
    match name {
        "project_list" => Ok(serde_json::to_value(core.store()?.projects()?)?),
        "project_get" => {
            let id = text(&arguments, "project_id")?;
            let project = core.store()?.project(id)?.context("Unknown project")?;
            Ok(json!({"project":project,"base_url":core.project_base_url(id)?}))
        }
        "project_scan" => {
            let project = core
                .store()?
                .project(text(&arguments, "project_id")?)?
                .context("Unknown project")?;
            let report = core.scan(&project)?;
            Ok(
                json!({"project_id":report.project_id,"scanned_at":report.scanned_at,"candidate_count":report.candidates.len(),"frameworks":report.frameworks,"files_inspected":report.files_inspected,"warnings":report.warnings.iter().take(50).collect::<Vec<_>>(),"warning_count":report.warnings.len()}),
            )
        }
        "test_list" | "test_get" => {
            let id = text(&arguments, "project_id")?;
            ensure!(core.store()?.project(id)?.is_some(), "Unknown project");
            let report = core
                .store()?
                .latest_scan(id)?
                .context("Scan this project first")?;
            if name == "test_get" {
                let candidate = report
                    .candidates
                    .iter()
                    .find(|candidate| {
                        arguments.get("test_id").and_then(Value::as_str) == Some(&candidate.id)
                    })
                    .context("Unknown candidate")?;
                Ok(serde_json::to_value(candidate)?)
            } else {
                let offset = arguments
                    .get("offset")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(usize::MAX as u64) as usize;
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100)
                    .clamp(1, 200) as usize;
                Ok(
                    json!({"total":report.candidates.len(),"offset":offset,"candidates":report.candidates.iter().skip(offset).take(limit).collect::<Vec<_>>(),"frameworks":report.frameworks}),
                )
            }
        }
        "test_plan" | "test_run" => {
            let id = text(&arguments, "project_id")?;
            if name == "test_run" {
                ensure!(
                    allowed.iter().any(|allowed| allowed == id),
                    "Execution is not authorized for this project. The local operator must start MCP with --allow-execution PROJECT_ID after reviewing its test commands."
                );
            }
            let ids: Vec<String> = arguments
                .get("test_ids")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()?
                .unwrap_or_default();
            let options = RunOptions {
                actor: "external_mcp_client".into(),
                ..RunOptions::default()
            };
            let plan = core.plan_run(id, &ids, options)?;
            if name == "test_plan" {
                let offset = offset(&arguments);
                let commands: Vec<_> = plan.commands.iter().skip(offset).take(20).collect();
                Ok(
                    json!({"id":plan.id,"project_id":plan.project_id,"planned_at":plan.planned_at,"options":plan.options,"revision":plan.revision,"command_count":plan.commands.len(),"offset":offset,"commands":commands}),
                )
            } else {
                Ok(run_summary(&core.start_run(plan, true)?))
            }
        }
        "test_result" | "test_cancel" | "test_evidence" | "test_output" => {
            let id = text(&arguments, "run_id")?;
            let run = core.run(id)?.context("Unknown run")?;
            match name {
                "test_cancel" => {
                    ensure!(
                        allowed.iter().any(|id| id == &run.project_id),
                        "Cancellation is not authorized for this project"
                    );
                    Ok(run_summary(&core.cancel_run(id)?))
                }
                "test_evidence" => Ok(
                    json!({"run_id":id,"artifacts":run.artifacts.iter().skip(offset(&arguments)).take(50).collect::<Vec<_>>(),"total":run.artifacts.len(),"status":run.status}),
                ),
                "test_output" => Ok(serde_json::to_value(core.run_output(
                    id,
                    arguments.get("offset").and_then(Value::as_u64).unwrap_or(0),
                    16384,
                )?)?),
                _ => {
                    let mut summary = run_summary(&run);
                    summary["results"] = serde_json::to_value(
                        run.results
                            .iter()
                            .skip(offset(&arguments))
                            .take(100)
                            .collect::<Vec<_>>(),
                    )?;
                    summary["offset"] = json!(offset(&arguments));
                    Ok(summary)
                }
            }
        }
        "test_artifact" => {
            let run_id=text(&arguments,"run_id")?;
            let path=text(&arguments,"path")?;
            let start=arguments.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let (bytes,total)=core.read_artifact(run_id,std::path::Path::new(path),start,32768)?;
            Ok(json!({"run_id":run_id,"path":path,"offset":start,"next_offset":start+bytes.len() as u64,"total_bytes":total,"encoding":"base64","data":base64::engine::general_purpose::STANDARD.encode(bytes)}))
        }
        "test_recommend" => {
            let paths:Vec<std::path::PathBuf>=serde_json::from_value(arguments.get("changed_files").cloned().unwrap_or_else(||json!([])))?;
            let report=core.recommend_tests(text(&arguments,"project_id")?,&paths)?;
            Ok(json!({"project_id":report.project_id,"tests":report.tests.iter().skip(offset(&arguments)).take(100).collect::<Vec<_>>(),"total":report.tests.len(),"exhaustive":report.exhaustive,"limitations":report.limitations}))
        }
        "test_coverage" => Ok(serde_json::to_value(core.coverage(text(&arguments,"project_id")?)?)?),
        "verification_status" | "verification_report" => {
            let report=core.verification_report(text(&arguments,"project_id")?)?;
            Ok(json!({"project_id":report.project_id,"status":report.status,"source":report.source,"checks":report.checks.iter().skip(offset(&arguments)).take(100).collect::<Vec<_>>(),"check_count":report.checks.len(),"warnings":report.warnings,"generated_at":report.generated_at}))
        }
        "recording_list" | "recording_get" | "recording_generate_test" => {
            let project=text(&arguments,"project_id")?;
            let mut worker=verifact_core::BrowserWorker::spawn(&core.recording_data_dir(project)?)?;
            let method=match name {"recording_list"=>"recording.list","recording_get"=>"recording.get",_=>"recording.generate_test"};
            let mut params=json!({"projectId":project});
            if name!="recording_list" {params["id"]=json!(text(&arguments,"recording_id")?);}
            let mut value=worker.request(method,params)?;
            if name=="recording_get" {
                if let Some(steps)=value.get("steps").and_then(Value::as_array) {
                    let total=steps.len();let steps:Vec<_>=steps.iter().skip(offset(&arguments)).take(100).cloned().collect();
                    value["steps"]=json!(steps);value["step_count"]=json!(total);
                }
            } else if name=="recording_generate_test" {
                if let Some(source)=value.get("source").and_then(Value::as_str) {
                    let total=source.chars().count();let source:String=source.chars().skip(offset(&arguments)).take(16384).collect();
                    value["source"]=json!(source);value["source_chars"]=json!(total);
                }
            } else if let Some(items)=value.get("recordings").and_then(Value::as_array) {
                let total=items.len();let items:Vec<_>=items.iter().skip(offset(&arguments)).take(100).cloned().collect();
                value["recordings"]=json!(items);value["total"]=json!(total);
            }
            Ok(value)
        }
        "test_history" => Ok(
            json!({"runs":core.run_history(text(&arguments,"project_id")?,20)?.iter().map(run_summary).collect::<Vec<_>>()}),
        ),
        _ => bail!("Unknown tool"),
    }
}

fn offset(arguments: &Value) -> usize {
    arguments
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}

fn run_summary(run: &RunRecord) -> Value {
    let mut counts = std::collections::BTreeMap::new();
    for result in &run.results {
        *counts.entry(result.status.as_str()).or_insert(0usize) += 1;
    }
    json!({"id":run.id,"project_id":run.project_id,"status":run.status,"queued_at":run.queued_at,"finished_at":run.finished_at,"duration_ms":run.duration_ms,"exit_code":run.exit_code,"result_count":run.results.len(),"result_status_counts":counts,"artifact_count":run.artifacts.len(),"message":run.message.as_ref().map(|message|message.chars().take(2000).collect::<String>()),"revision":run.revision,"working_tree":run.working_tree,"output_truncated":run.output_truncated})
}

fn tool_definitions() -> Vec<Value> {
    let string = json!({"type":"string","minLength":1});
    let project = json!({"project_id":string});
    let run = json!({"run_id":string,"offset":{"type":"integer","minimum":0}});
    let selection = json!({"project_id":string,"test_ids":{"type":"array","items":string,"maxItems":10000},"offset":{"type":"integer","minimum":0}});
    let list = json!({"project_id":string,"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}});
    [
        ("test_artifact","Read a bounded checksum-verified artifact chunk",json!({"run_id":string,"path":string,"offset":{"type":"integer","minimum":0}}),vec!["run_id","path"],true),
        ("test_recommend","Recommend files from explicit relationships and static heuristics; non-exhaustive",json!({"project_id":string,"changed_files":{"type":"array","items":string,"maxItems":1000},"offset":{"type":"integer","minimum":0}}),vec!["project_id"],true),
        ("test_coverage","Report separate available and unavailable coverage dimensions",project.clone(),vec!["project_id"],true),
        ("verification_status","Evaluate configured requirements against real current-source evidence",list.clone(),vec!["project_id"],true),
        ("verification_report","Read paginated verification checks and provenance",list.clone(),vec!["project_id"],true),
        ("recording_list","List saved journeys",list.clone(),vec!["project_id"],true),
        ("recording_get","Read a page of saved journey steps",json!({"project_id":string,"recording_id":string,"offset":{"type":"integer","minimum":0}}),vec!["project_id","recording_id"],true),
        ("recording_generate_test","Generate ordinary Playwright source without executing or exporting it",json!({"project_id":string,"recording_id":string,"offset":{"type":"integer","minimum":0}}),vec!["project_id","recording_id"],true),
        ("project_list","List registered local projects",json!({}),vec![],true),
        ("project_get","Get project settings",project.clone(),vec!["project_id"],true),
        ("project_scan","Passively refresh discovery; executes no project code",project.clone(),vec!["project_id"],false),
        ("test_list","Page through discovered file candidates",list,vec!["project_id"],true),
        ("test_get","Get one candidate file",json!({"project_id":string,"test_id":string}),vec!["project_id","test_id"],true),
        ("test_plan","Review safe argument-array commands without executing them",selection.clone(),vec!["project_id"],true),
        ("test_run","Execute selected files or the project under operator authorization",selection,vec!["project_id"],false),
        ("test_cancel","Cancel an authorized execution",run.clone(),vec!["run_id"],false),
        ("test_result","Read an actual execution record",run.clone(),vec!["run_id"],true),
        ("test_history","Read the latest twenty executions",project,vec!["project_id"],true),
        ("test_evidence","List managed evidence artifact references",run,vec!["run_id"],true),
        ("test_output","Read a bounded output chunk",json!({"run_id":string,"offset":{"type":"integer","minimum":0}}),vec!["run_id"],true),
    ].into_iter().map(|(name,description,properties,required,read_only)| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read_only,"destructiveHint":!read_only,"openWorldHint":!read_only}})).collect()
}
