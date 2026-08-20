use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use gpt_image_2_core::{shared_config_dir, VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SKIP_DAEMON_ENV: &str = "GPT_IMAGE_2_SKIP_DAEMON";
pub const DAEMON_HOST_ENV: &str = "GPT_IMAGE_2_DAEMON_HOST";
pub const DAEMON_PORT_ENV: &str = "GPT_IMAGE_2_DAEMON_PORT";
pub const DAEMON_NO_WAIT_ENV: &str = "GPT_IMAGE_2_DAEMON_NO_WAIT";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8787;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub version: String,
}

pub fn skip_daemon() -> bool {
    matches!(
        std::env::var(SKIP_DAEMON_ENV)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn no_wait() -> bool {
    matches!(
        std::env::var(DAEMON_NO_WAIT_ENV)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn daemon_host() -> String {
    let host = std::env::var(DAEMON_HOST_ENV).unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let host = host.trim();
    if host.is_empty() {
        DEFAULT_HOST.to_string()
    } else {
        host.to_string()
    }
}

pub fn daemon_port() -> u16 {
    std::env::var(DAEMON_PORT_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn daemon_base_url() -> String {
    format!("http://{}:{}", daemon_host(), daemon_port())
}

pub fn daemon_info_path() -> PathBuf {
    shared_config_dir().join("daemon.json")
}

pub fn daemon_log_path() -> PathBuf {
    shared_config_dir().join("daemon.log")
}

pub fn dispatch(argv: &[String]) -> i32 {
    let action = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .nth(1)
        .map(String::as_str)
        .unwrap_or("foreground");
    match action {
        "start" => print_and_status(start_background()),
        "stop" => print_and_status(stop()),
        "status" => print_and_status(status_payload()),
        "foreground" | "run" => run_foreground(),
        other => {
            let payload = json!({
                "ok": false,
                "error": {
                    "code": "invalid_command",
                    "message": format!("Unknown daemon command: {other}"),
                    "detail": { "usage": "gpt-image-2-skill daemon [start|stop|status|foreground]" }
                }
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            2
        }
    }
}

fn print_and_status(payload: Value) -> i32 {
    let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
    println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
    if ok {
        0
    } else {
        1
    }
}

pub fn run_foreground() -> i32 {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    unsafe {
        std::env::set_var("GPT_IMAGE_2_DAEMON", "1");
    }
    let host = daemon_host();
    let port = daemon_port();
    write_info(&DaemonInfo {
        pid: std::process::id(),
        host: host.clone(),
        port,
        version: VERSION.to_string(),
    });
    match gpt_image_2_web::run_api_only(host, port) {
        Ok(()) => 0,
        Err(error) => {
            let payload = json!({
                "ok": false,
                "error": {
                    "code": "daemon_failed",
                    "message": error.to_string(),
                }
            });
            eprintln!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
            1
        }
    }
}

pub fn ensure_running() -> Result<DaemonInfo, String> {
    if let Some(info) = healthy_info() {
        return Ok(info);
    }
    start_background_inner()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Some(info) = healthy_info() {
            return Ok(info);
        }
        thread::sleep(Duration::from_millis(150));
    }
    Err(format!(
        "Daemon did not become healthy on {}.",
        daemon_base_url()
    ))
}

fn start_background() -> Value {
    match start_background_inner() {
        Ok(info) => json!({
            "ok": true,
            "command": "daemon start",
            "daemon": info,
            "url": daemon_base_url(),
        }),
        Err(message) => json!({
            "ok": false,
            "error": { "code": "daemon_start_failed", "message": message }
        }),
    }
}

fn start_background_inner() -> Result<DaemonInfo, String> {
    if let Some(info) = healthy_info() {
        return Ok(info);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let log_path = daemon_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| error.to_string())?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .arg("foreground")
        .env("GPT_IMAGE_2_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    command.spawn().map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Some(info) = healthy_info() {
            return Ok(info);
        }
        thread::sleep(Duration::from_millis(150));
    }
    Err(format!(
        "Started daemon process but {} did not respond. See {}.",
        daemon_base_url(),
        log_path.display()
    ))
}

fn stop() -> Value {
    let Some(info) = read_info() else {
        return json!({
            "ok": true,
            "command": "daemon stop",
            "stopped": false,
            "message": "Daemon is not running."
        });
    };
    let _ = kill_pid(info.pid);
    let _ = fs::remove_file(daemon_info_path());
    json!({
        "ok": true,
        "command": "daemon stop",
        "stopped": true,
        "pid": info.pid,
    })
}

fn status_payload() -> Value {
    match healthy_info() {
        Some(info) => json!({
            "ok": true,
            "command": "daemon status",
            "running": true,
            "daemon": info,
            "url": format!("http://{}:{}/api", info.host, info.port),
        }),
        None => json!({
            "ok": true,
            "command": "daemon status",
            "running": false,
            "url": daemon_base_url(),
        }),
    }
}

fn healthy_info() -> Option<DaemonInfo> {
    let info = read_info().or_else(|| {
        Some(DaemonInfo {
            pid: 0,
            host: daemon_host(),
            port: daemon_port(),
            version: VERSION.to_string(),
        })
    })?;
    if probe(&info.host, info.port) {
        Some(info)
    } else {
        None
    }
}

pub fn probe(host: &str, port: u16) -> bool {
    let url = format!("http://{host}:{port}/api/queue");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    client
        .get(url)
        .header("Host", format!("{host}:{port}"))
        .send()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

fn read_info() -> Option<DaemonInfo> {
    let raw = fs::read_to_string(daemon_info_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_info(info: &DaemonInfo) {
    let path = daemon_info_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(payload) = serde_json::to_string_pretty(info) {
        let _ = fs::write(path, payload);
    }
}

fn kill_pid(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}
