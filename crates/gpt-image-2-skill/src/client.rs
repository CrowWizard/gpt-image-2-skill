use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use gpt_image_2_core::{
    Cli, Commands, EditImageArgs, EditRequest, GenerateImageArgs, GenerateRequest, ImagesSubcommand,
    UploadFile,
};
use serde_json::{Value, json};

use crate::daemon;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const POLL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

pub fn run_images(cli: &Cli) -> i32 {
    let Commands::Images(command) = &cli.command else {
        return fallback_local(cli);
    };
    match run_images_via_daemon(cli, &command.images_command) {
        Ok(payload) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{\"ok\":false}".into())
            );
            if payload.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                0
            } else {
                1
            }
        }
        Err(message) => {
            let payload = json!({
                "ok": false,
                "error": {
                    "code": "daemon_request_failed",
                    "message": message,
                }
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            1
        }
    }
}

fn fallback_local(_cli: &Cli) -> i32 {
    1
}

fn run_images_via_daemon(cli: &Cli, command: &ImagesSubcommand) -> Result<Value, String> {
    let info = daemon::ensure_running()?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let (path, body, out) = match command {
        ImagesSubcommand::Generate(args) => (
            "/api/images/generate",
            serde_json::to_value(generate_request(cli, args)).map_err(|error| error.to_string())?,
            args.shared.out.clone(),
        ),
        ImagesSubcommand::Edit(args) => (
            "/api/images/edit",
            serde_json::to_value(edit_request(cli, args)?).map_err(|error| error.to_string())?,
            args.shared.out.clone(),
        ),
    };
    let url = format!("http://{}:{}{path}", info.host, info.port);
    let response = client
        .post(&url)
        .header("Host", format!("{}:{}", info.host, info.port))
        .json(&body)
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let mut payload: Value = response.json().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Ok(json!({
            "ok": false,
            "error": payload.get("error").cloned().unwrap_or(json!({
                "code": "http_error",
                "message": format!("Daemon returned {status}"),
            })),
        }));
    }
    let job_id = payload
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Daemon enqueue response did not include job_id.".to_string())?
        .to_string();
    if cli.no_wait || daemon::no_wait() {
        remember_ticket(&job_id, &out);
        payload["ok"] = json!(true);
        payload["command"] = json!(command_name(command));
        payload["queued"] = json!(true);
        payload["job_id"] = json!(job_id);
        payload["out"] = json!(out);
        payload["status"] = payload
            .get("status")
            .cloned()
            .unwrap_or_else(|| json!("queued"));
        payload["ticket"] = json!({
            "job_id": job_id,
            "out": out,
        });
        return Ok(payload);
    }
    let job = wait_for_job(&client, &info, &job_id)?;
    let copied = copy_outputs_to(&job, Path::new(&out))?;
    Ok(json!({
        "ok": job_ok(&job),
        "command": command_name(command),
        "provider": job.get("provider").cloned().unwrap_or(Value::Null),
        "job_id": job_id,
        "queued": true,
        "status": job.get("status").cloned().unwrap_or(Value::Null),
        "output": copied,
        "job": job,
        "error": job.get("error").cloned().unwrap_or(Value::Null),
    }))
}

fn command_name(command: &ImagesSubcommand) -> &'static str {
    match command {
        ImagesSubcommand::Generate(_) => "images generate",
        ImagesSubcommand::Edit(_) => "images edit",
    }
}

fn generate_request(cli: &Cli, args: &GenerateImageArgs) -> GenerateRequest {
    let shared = &args.shared;
    GenerateRequest {
        prompt: shared.prompt.clone(),
        provider: provider_arg(cli),
        size: shared.size.clone(),
        format: shared.output_format.map(|value| value.as_str().to_string()),
        quality: shared.quality.map(|value| value.as_str().to_string()),
        background: Some(shared.background.as_str().to_string()),
        n: shared.n,
        compression: shared.output_compression,
        moderation: shared.moderation.map(|value| value.as_str().to_string()),
        storage_targets: None,
        fallback_targets: None,
    }
}

fn edit_request(cli: &Cli, args: &EditImageArgs) -> Result<EditRequest, String> {
    let shared = &args.shared;
    Ok(EditRequest {
        prompt: shared.prompt.clone(),
        provider: provider_arg(cli),
        size: shared.size.clone(),
        format: shared.output_format.map(|value| value.as_str().to_string()),
        quality: shared.quality.map(|value| value.as_str().to_string()),
        background: Some(shared.background.as_str().to_string()),
        n: shared.n,
        compression: shared.output_compression,
        input_fidelity: args
            .input_fidelity
            .map(|value| value.as_str().to_string()),
        moderation: shared.moderation.map(|value| value.as_str().to_string()),
        storage_targets: None,
        fallback_targets: None,
        refs: args
            .ref_image
            .iter()
            .map(|path| read_upload(path))
            .collect::<Result<Vec<_>, _>>()?,
        mask: args.mask.as_deref().map(read_upload).transpose()?,
        selection_hint: None,
    })
}

fn provider_arg(cli: &Cli) -> Option<String> {
    let value = cli.provider.trim();
    if value.is_empty() || value == "auto" {
        None
    } else {
        Some(value.to_string())
    }
}

fn read_upload(path: &str) -> Result<UploadFile, String> {
    let bytes = fs::read(path).map_err(|error| format!("Unable to read {path}: {error}"))?;
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image.png")
        .to_string();
    Ok(UploadFile { name, bytes })
}

fn wait_for_job(
    client: &reqwest::blocking::Client,
    info: &daemon::DaemonInfo,
    job_id: &str,
) -> Result<Value, String> {
    let url = format!("http://{}:{}/api/jobs/{job_id}", info.host, info.port);
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut last_events = 0usize;
    loop {
        let response = client
            .get(&url)
            .header("Host", format!("{}:{}", info.host, info.port))
            .send()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("Polling job failed: {}", response.status()));
        }
        let payload: Value = response.json().map_err(|error| error.to_string())?;
        if let Some(events) = payload.get("events").and_then(Value::as_array) {
            for event in events.iter().skip(last_events) {
                eprintln!("{}", event);
            }
            last_events = events.len();
        }
        let job = payload
            .get("job")
            .cloned()
            .ok_or_else(|| "Job payload missing.".to_string())?;
        let status = job.get("status").and_then(Value::as_str).unwrap_or("");
        if is_terminal(status) {
            return Ok(job);
        }
        if Instant::now() >= deadline {
            return Err(format!("Timed out waiting for job {job_id}."));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn remember_ticket(job_id: &str, out: &str) {
    let path = ticket_store_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut map = read_ticket_map();
    map.insert(job_id.to_string(), json!(out));
    if let Ok(payload) = serde_json::to_string_pretty(&map) {
        let _ = fs::write(path, payload);
    }
}

pub(crate) fn lookup_ticket_out(job_id: &str) -> Option<String> {
    read_ticket_map()
        .get(job_id)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn ticket_store_path() -> PathBuf {
    gpt_image_2_core::shared_config_dir().join("client-outs.json")
}

fn read_ticket_map() -> serde_json::Map<String, Value> {
    fs::read_to_string(ticket_store_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub(crate) fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "partial_failed" | "failed" | "cancelled" | "canceled"
    )
}

pub(crate) fn job_ok(job: &Value) -> bool {
    matches!(
        job.get("status").and_then(Value::as_str),
        Some("completed" | "partial_failed")
    )
}

pub(crate) fn copy_outputs_to(job: &Value, out: &Path) -> Result<Value, String> {
    let mut files = job
        .get("outputs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty()
        && let Some(path) = job.get("output_path").and_then(Value::as_str)
    {
        files.push(json!({ "index": 0, "path": path }));
    }
    if files.is_empty() {
        return Ok(json!({ "path": Value::Null, "files": [] }));
    }
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut copied = Vec::new();
    for (offset, file) in files.iter().enumerate() {
        let Some(source) = file.get("path").and_then(Value::as_str) else {
            continue;
        };
        let dest = if files.len() == 1 {
            out.to_path_buf()
        } else {
            numbered_out(out, offset)
        };
        fs::copy(source, &dest).map_err(|error| {
            format!(
                "Unable to copy {} -> {}: {error}",
                source,
                dest.display()
            )
        })?;
        let bytes = fs::metadata(&dest).map(|meta| meta.len()).unwrap_or(0);
        copied.push(json!({
            "index": file.get("index").cloned().unwrap_or(json!(offset)),
            "path": dest.display().to_string(),
            "bytes": bytes,
        }));
    }
    let primary = copied
        .iter()
        .find(|file| file.get("index").and_then(Value::as_u64) == Some(0))
        .or(copied.first())
        .and_then(|file| file.get("path").cloned())
        .unwrap_or(Value::Null);
    Ok(json!({
        "path": primary,
        "files": copied,
    }))
}

fn numbered_out(out: &Path, index: usize) -> PathBuf {
    let stem = out
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let ext = out
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".png".to_string());
    out.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}-{}{ext}", index + 1))
}
