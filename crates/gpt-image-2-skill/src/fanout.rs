use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Instant,
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::SKIP_DAEMON_ENV;

const DEFAULT_MAX_PARALLEL: usize = 10;
const MAX_PARALLEL_CAP: usize = 10;
const DEFAULT_RETRIES: u32 = 2;

#[derive(Debug, Deserialize)]
struct FanoutJob {
    #[serde(default)]
    name: Option<String>,
    prompt: String,
    #[serde(default)]
    ref_image: RefImages,
    out: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    input_fidelity: Option<String>,
    #[serde(default)]
    background: Option<String>,
    #[serde(default)]
    mask: Option<String>,
    #[serde(default)]
    compression: Option<u8>,
    #[serde(default)]
    moderation: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum RefImages {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl RefImages {
    fn paths(&self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            Self::One(path) => {
                if path.is_empty() {
                    Vec::new()
                } else {
                    vec![path.clone()]
                }
            }
            Self::Many(paths) => paths
                .iter()
                .filter(|path| !path.is_empty())
                .cloned()
                .collect(),
        }
    }
}

struct PreparedJob {
    index: usize,
    name: String,
    out: String,
    child_args: Vec<String>,
}

struct TaskResult {
    index: usize,
    name: String,
    out: String,
    child_args: Vec<String>,
    attempt: u32,
    exit_code: i32,
    ok: bool,
    payload: Value,
}

pub fn is_command(argv: &[String]) -> bool {
    let positional: Vec<&str> = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .collect();
    positional.first().copied() == Some("images") && positional.get(1).copied() == Some("fanout")
}

pub fn dispatch(argv: &[String]) -> i32 {
    match run(argv) {
        Ok(payload) => {
            let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{\"ok\":false}".into())
            );
            if ok {
                0
            } else {
                1
            }
        }
        Err(message) => {
            let payload = json!({
                "ok": false,
                "command": "images fanout",
                "error": { "code": "fanout_failed", "message": message }
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            1
        }
    }
}

fn run(argv: &[String]) -> Result<Value, String> {
    let (globals, jobs_path, max_parallel, retries) = parse_args(argv)?;
    let raw = fs::read_to_string(&jobs_path)
        .map_err(|error| format!("Unable to read {}: {error}", jobs_path.display()))?;
    let jobs: Vec<FanoutJob> = serde_json::from_str(&raw)
        .map_err(|error| format!("Jobs file must be a JSON array: {error}"))?;
    if jobs.is_empty() {
        return Err("Jobs file is empty.".to_string());
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let prepared: Vec<PreparedJob> = jobs
        .into_iter()
        .enumerate()
        .map(|(index, job)| prepare_job(index, job, &globals))
        .collect::<Result<Vec<_>, _>>()?;
    let started = Instant::now();
    let results = run_pool(&exe, prepared, max_parallel, retries)?;
    let all_ok = results.iter().all(|task| task.ok);
    Ok(json!({
        "ok": all_ok,
        "command": "images fanout",
        "jobs_file": jobs_path.display().to_string(),
        "max_parallel": max_parallel,
        "retries": retries,
        "elapsed_ms": started.elapsed().as_millis(),
        "tasks": results.iter().map(task_payload).collect::<Vec<_>>(),
    }))
}

fn parse_args(argv: &[String]) -> Result<(Vec<String>, PathBuf, usize, u32), String> {
    let mut globals = Vec::new();
    let mut jobs_path = None;
    let mut max_parallel = DEFAULT_MAX_PARALLEL;
    let mut retries = DEFAULT_RETRIES;
    let mut json_events = false;
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "images" | "fanout" => i += 1,
            "--jobs" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| "--jobs requires a file path".to_string())?;
                jobs_path = Some(PathBuf::from(value));
                i += 2;
            }
            "--max-parallel" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| "--max-parallel requires a number".to_string())?;
                let parsed: usize = value
                    .parse()
                    .map_err(|_| format!("Invalid --max-parallel: {value}"))?;
                max_parallel = parsed.clamp(1, MAX_PARALLEL_CAP);
                i += 2;
            }
            "--retries" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| "--retries requires a number".to_string())?;
                retries = value
                    .parse()
                    .map_err(|_| format!("Invalid --retries: {value}"))?;
                i += 2;
            }
            "--json" => i += 1,
            "--json-events" => {
                json_events = true;
                i += 1;
            }
            "--provider"
            | "--api-key"
            | "--config"
            | "--auth-file"
            | "--endpoint"
            | "--openai-api-base"
            | "--model"
            | "-m" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                globals.push(arg.to_string());
                globals.push(value.clone());
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown fanout flag: {other}"));
            }
            _ => i += 1,
        }
    }
    if json_events {
        globals.push("--json-events".to_string());
    }
    let jobs_path = jobs_path.ok_or_else(|| "images fanout requires --jobs <file.json>".to_string())?;
    Ok((globals, jobs_path, max_parallel, retries))
}

fn prepare_job(
    index: usize,
    job: FanoutJob,
    globals: &[String],
) -> Result<PreparedJob, String> {
    if job.prompt.trim().is_empty() {
        return Err(format!("Job {index} is missing prompt."));
    }
    if job.out.trim().is_empty() {
        return Err(format!("Job {index} is missing out."));
    }
    let name = job
        .name
        .clone()
        .unwrap_or_else(|| {
            Path::new(&job.out)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("job")
                .to_string()
        });
    let refs = job.ref_image.paths();
    let mut child_args = vec!["--json".to_string()];
    child_args.extend(globals.iter().cloned());
    if refs.is_empty() {
        child_args.extend(["images".into(), "generate".into()]);
    } else {
        child_args.extend(["images".into(), "edit".into()]);
        for path in &refs {
            child_args.push("--ref-image".into());
            child_args.push(path.clone());
        }
    }
    child_args.push("--prompt".into());
    child_args.push(job.prompt);
    child_args.push("--out".into());
    child_args.push(job.out.clone());
    push_opt(&mut child_args, "--size", job.size);
    push_opt(&mut child_args, "--format", job.format);
    push_opt(&mut child_args, "--quality", job.quality);
    push_opt(&mut child_args, "--input-fidelity", job.input_fidelity);
    push_opt(&mut child_args, "--background", job.background);
    push_opt(&mut child_args, "--mask", job.mask);
    push_opt(&mut child_args, "--moderation", job.moderation);
    if let Some(compression) = job.compression {
        child_args.push("--compression".into());
        child_args.push(compression.to_string());
    }
    Ok(PreparedJob {
        index,
        name,
        out: job.out,
        child_args,
    })
}

fn push_opt(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn run_pool(
    exe: &Path,
    jobs: Vec<PreparedJob>,
    max_parallel: usize,
    retries: u32,
) -> Result<Vec<TaskResult>, String> {
    let total = jobs.len();
    let mut queue: VecDeque<(PreparedJob, u32)> =
        jobs.into_iter().map(|job| (job, 1u32)).collect();
    let (tx, rx) = mpsc::channel::<TaskResult>();
    let mut running = 0usize;
    let mut done: Vec<Option<TaskResult>> = (0..total).map(|_| None).collect();

    while done.iter().any(|slot| slot.is_none()) || running > 0 {
        while running < max_parallel {
            let Some((job, attempt)) = queue.pop_front() else {
                break;
            };
            let exe = exe.to_path_buf();
            let tx = tx.clone();
            running += 1;
            thread::spawn(move || {
                let result = run_child(&exe, job, attempt);
                let _ = tx.send(result);
            });
        }
        let result = rx
            .recv()
            .map_err(|_| "Fanout worker channel closed.".to_string())?;
        running -= 1;
        if !result.ok && result.attempt <= retries {
            queue.push_back((
                PreparedJob {
                    index: result.index,
                    name: result.name.clone(),
                    out: result.out.clone(),
                    child_args: result.child_args.clone(),
                },
                result.attempt + 1,
            ));
            continue;
        }
        let index = result.index;
        done[index] = Some(result);
    }

    Ok(done.into_iter().map(|slot| slot.expect("filled")).collect())
}

fn run_child(exe: &Path, job: PreparedJob, attempt: u32) -> TaskResult {
    let child_args = job.child_args.clone();
    let output = Command::new(exe)
        .args(&job.child_args)
        .env(SKIP_DAEMON_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output();
    match output {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let payload = serde_json::from_str::<Value>(stdout.trim()).unwrap_or_else(|_| {
                json!({
                    "ok": false,
                    "error": {
                        "code": "child_output_invalid",
                        "message": stdout.trim(),
                    }
                })
            });
            let ok = output.status.success()
                && payload
                    .get("ok")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            TaskResult {
                index: job.index,
                name: job.name,
                out: job.out,
                child_args,
                attempt,
                exit_code,
                ok,
                payload,
            }
        }
        Err(error) => TaskResult {
            index: job.index,
            name: job.name,
            out: job.out,
            child_args,
            attempt,
            exit_code: 1,
            ok: false,
            payload: json!({
                "ok": false,
                "error": { "code": "child_spawn_failed", "message": error.to_string() },
            }),
        },
    }
}

fn task_payload(task: &TaskResult) -> Value {
    json!({
        "name": task.name,
        "out": task.out,
        "ok": task.ok,
        "attempts": task.attempt,
        "exit_code": task.exit_code,
        "result": task.payload,
    })
}
