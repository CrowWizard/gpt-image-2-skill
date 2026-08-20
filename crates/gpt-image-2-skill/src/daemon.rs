use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use gpt_image_2_core::{shared_config_dir, VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_default()
            );
            2
        }
    }
}

fn print_and_status(payload: Value) -> i32 {
    let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
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
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_default()
            );
            1
        }
    }
}

pub fn ensure_running() -> Result<DaemonInfo, String> {
    if let Some(info) = healthy_info() {
        eprintln!(
            "gpt-image-2-skill: reusing daemon pid={} http://{}:{}/api",
            info.pid, info.host, info.port
        );
        return Ok(info);
    }
    eprintln!(
        "gpt-image-2-skill: starting shared daemon on {}",
        daemon_base_url()
    );
    start_background_inner()
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
    let _lock = acquire_start_lock();
    if let Some(info) = healthy_info() {
        return Ok(info);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let log_path = daemon_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    spawn_detached_daemon(&exe, &log_path)?;
    wait_until_healthy(&log_path)
}

fn wait_until_healthy(log_path: &Path) -> Result<DaemonInfo, String> {
    let deadline = Instant::now() + Duration::from_secs(20);
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

struct StartLock {
    path: Option<PathBuf>,
}

impl Drop for StartLock {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

fn acquire_start_lock() -> StartLock {
    let path = shared_config_dir().join("daemon.start.lock");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return StartLock { path: Some(path) },
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if healthy_info().is_some() {
                    return StartLock { path: None };
                }
                if Instant::now() >= deadline {
                    let _ = fs::remove_file(&path);
                    match fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                    {
                        Ok(_) => return StartLock { path: Some(path) },
                        Err(_) => return StartLock { path: None },
                    }
                }
                thread::sleep(Duration::from_millis(150));
            }
            Err(_) => return StartLock { path: None },
        }
    }
}

fn spawn_detached_daemon(exe: &Path, log_path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
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
        command.spawn().map_err(|error| error.to_string())?;
        Ok(())
    }
    #[cfg(windows)]
    {
        spawn_windows_daemon(exe, log_path)
    }
}

#[cfg(windows)]
fn spawn_windows_daemon(exe: &Path, log_path: &Path) -> Result<(), String> {
    // PowerShell `& $cli` and Codex put the client in a Job Object with
    // kill-on-close. CREATE_BREAKAWAY_FROM_JOB only works when the job allows
    // it; the previous in-job fallback looked like a successful start, then
    // died when the client exited, so the next page spawned a new exe.
    // Never keep a daemon inside that job.
    if spawn_windows_breakaway(exe, log_path).is_ok() {
        return Ok(());
    }
    spawn_windows_via_wmi(exe, log_path)
}

#[cfg(windows)]
fn spawn_windows_breakaway(exe: &Path, log_path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x00000400;

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| error.to_string())?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .arg("foreground")
        .env("GPT_IMAGE_2_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .creation_flags(
            CREATE_UNICODE_ENVIRONMENT
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_NO_WINDOW
                | CREATE_BREAKAWAY_FROM_JOB,
        );
    command.spawn().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn spawn_windows_via_wmi(exe: &Path, log_path: &Path) -> Result<(), String> {
    let launcher = write_windows_launcher(exe, log_path)?;
    let launcher_s = launcher.display().to_string();
    let command_line = format!(
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{launcher_s}\""
    );
    let mut last_err = String::from("WMI Win32_Process.Create failed");
    if let Err(error) = wmi_create_process_cim(&command_line) {
        last_err = error;
        if let Err(error) = wmi_create_process_wmic(&command_line) {
            last_err = error;
        } else {
            return Ok(());
        }
        return Err(last_err);
    }
    Ok(())
}

#[cfg(windows)]
fn write_windows_launcher(exe: &Path, log_path: &Path) -> Result<PathBuf, String> {
    let launcher = shared_config_dir().join("daemon-launch.ps1");
    if let Some(parent) = launcher.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut body = String::from("$ErrorActionPreference = 'Continue'\n");
    for (key, value) in std::env::vars() {
        if !is_powershell_env_name(&key) {
            continue;
        }
        body.push_str(&format!(
            "$env:{key} = {}\n",
            powershell_single_quote(&value)
        ));
    }
    body.push_str("$env:GPT_IMAGE_2_DAEMON = '1'\n");
    body.push_str(&format!(
        "& {} daemon foreground *>> {}\n",
        powershell_single_quote(&exe.display().to_string()),
        powershell_single_quote(&log_path.display().to_string())
    ));
    fs::write(&launcher, body).map_err(|error| error.to_string())?;
    Ok(launcher)
}

#[cfg(windows)]
fn is_powershell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(windows)]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn wmi_create_process_cim(command_line: &str) -> Result<(), String> {
    let escaped = command_line.replace('\'', "''");
    let script = format!(
        "$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ CommandLine = '{escaped}' }}; if ($null -eq $r -or $r.ReturnValue -ne 0) {{ exit 1 }}"
    );
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Invoke-CimMethod Win32_Process.Create failed".to_string())
    }
}

#[cfg(windows)]
fn wmi_create_process_wmic(command_line: &str) -> Result<(), String> {
    let output = Command::new("wmic")
        .args(["process", "call", "create", command_line])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.to_ascii_lowercase().contains("returnvalue = 0") {
        Ok(())
    } else {
        Err(format!(
            "wmic process call create failed: {}",
            stdout.trim()
        ))
    }
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
    let addr = format!("{host}:{port}").parse().ok();
    if let Some(addr) = addr {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_err() {
            return false;
        }
    }
    let url = format!("http://{host}:{port}/api/queue");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    client
        .get(url)
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
