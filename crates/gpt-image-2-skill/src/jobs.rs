use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{client, daemon};

#[derive(Debug, Deserialize)]
struct Ticket {
    job_id: String,
    #[serde(default)]
    out: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

pub fn is_command(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .next()
        .map(String::as_str)
        == Some("jobs")
}

pub fn dispatch(argv: &[String]) -> i32 {
    let action = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .nth(1)
        .map(String::as_str)
        .unwrap_or("status");
    match action {
        "status" => print_status(run_status(argv)),
        other => {
            let payload = json!({
                "ok": false,
                "error": {
                    "code": "invalid_command",
                    "message": format!("Unknown jobs command: {other}"),
                    "detail": { "usage": "gpt-image-2-skill jobs status [--id JOB]... [--file tickets.json]" }
                }
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            2
        }
    }
}

fn print_status(result: Result<Value, String>) -> i32 {
    match result {
        Ok(payload) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{\"ok\":false}".into())
            );
            0
        }
        Err(message) => {
            let payload = json!({
                "ok": false,
                "command": "jobs status",
                "error": { "code": "jobs_status_failed", "message": message }
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            1
        }
    }
}

fn run_status(argv: &[String]) -> Result<Value, String> {
    let tickets = parse_tickets(argv)?;
    if tickets.is_empty() {
        return Err("jobs status requires --id JOB or --file tickets.json".to_string());
    }
    let info = daemon::ensure_running()?;
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let mut tasks = Vec::new();
    let mut pending = 0usize;
    let mut completed = 0usize;
    let mut failed = 0usize;
    for ticket in tickets {
        let task = poll_ticket(&http, &info, &ticket)?;
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let lookup_failed = task
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            == Some("job_lookup_failed");
        if lookup_failed {
            failed += 1;
        } else if client::is_terminal(status) {
            if task.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                completed += 1;
            } else {
                failed += 1;
            }
        } else {
            pending += 1;
        }
        tasks.push(task);
    }
    Ok(json!({
        "ok": true,
        "command": "jobs status",
        "pending": pending,
        "completed": completed,
        "failed": failed,
        "done": pending == 0,
        "all_ok": pending == 0 && failed == 0,
        "tasks": tasks,
    }))
}

fn parse_tickets(argv: &[String]) -> Result<Vec<Ticket>, String> {
    let mut ids = Vec::new();
    let mut file: Option<PathBuf> = None;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--id" | "--job-id" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| "--id requires a job id".to_string())?;
                ids.push(Ticket {
                    job_id: value.clone(),
                    out: None,
                    name: None,
                });
                i += 2;
            }
            "--file" | "--jobs" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| "--file requires a path".to_string())?;
                file = Some(PathBuf::from(value));
                i += 2;
            }
            _ => i += 1,
        }
    }
    if let Some(path) = file {
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("Unable to read {}: {error}", path.display()))?;
        let parsed: Vec<Ticket> = serde_json::from_str(&raw)
            .map_err(|error| format!("Ticket file must be a JSON array of {{job_id, out}}: {error}"))?;
        ids.extend(parsed);
    }
    Ok(ids)
}

fn poll_ticket(
    http: &reqwest::blocking::Client,
    info: &daemon::DaemonInfo,
    ticket: &Ticket,
) -> Result<Value, String> {
    let url = format!("http://{}:{}/api/jobs/{}", info.host, info.port, ticket.job_id);
    let response = http
        .get(&url)
        .send()
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Ok(json!({
            "job_id": ticket.job_id,
            "name": ticket.name,
            "ok": false,
            "status": "unknown",
            "error": {
                "code": "job_lookup_failed",
                "message": format!("HTTP {}", response.status()),
            }
        }));
    }
    let payload: Value = response.json().map_err(|error| error.to_string())?;
    let job = payload.get("job").cloned().unwrap_or(Value::Null);
    let status = job
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let ok = client::job_ok(&job);
    let out = ticket
        .out
        .clone()
        .or_else(|| client::lookup_ticket_out(&ticket.job_id));
    let mut copied = Value::Null;
    if client::is_terminal(&status) && ok {
        if let Some(dest) = out.as_deref() {
            copied = client::copy_outputs_to(&job, Path::new(dest))?;
        }
    }
    Ok(json!({
        "job_id": ticket.job_id,
        "name": ticket.name,
        "out": out,
        "ok": ok,
        "status": status,
        "output": copied,
        "error": job.get("error").cloned().unwrap_or(Value::Null),
        "job": job,
    }))
}
