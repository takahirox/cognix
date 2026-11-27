//! Startup and ownership for the CLI's local Ollama server.
use cognix::{Cancellation, Error, provider::DEFAULT_OLLAMA_URL, terminal};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::{
    error::Error as _,
    fs::File,
    io,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout_at};

const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const VERSION_LIMIT: usize = 64 * 1024;

// Holding this guard owns the child and its process group. A reused server never
// gets a guard, even when another app wins the race to bind the default port.
pub(super) struct OwnedServer(Child);

impl OwnedServer {
    fn spawn(mut command: Command, endpoint: &str) -> Result<Self, Error> {
        let logs = tempfile::Builder::new()
            .prefix("cognix-ollama.")
            .tempdir()
            .map_err(|e| Error::Provider(format!("Cannot create an Ollama log directory: {e}")))?;
        let path = logs.path().join("ollama.log");
        let log_error = |e| Error::Provider(format!("Cannot open the Ollama startup log: {e}"));
        let log = File::create(&path).map_err(log_error)?;
        let child = command
            .arg("serve")
            .env("OLLAMA_HOST", endpoint.trim_start_matches("http://"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(log_error)?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                Error::Provider(if e.kind() == io::ErrorKind::NotFound {
                    "ollama command not found. Install Ollama and put it on PATH, or start the Ollama app".into()
                } else {
                    format!("Cannot run 'ollama serve': {e}. Check the Ollama installation and permissions")
                })
            })?;
        let server = Self(child);
        let _ = logs.keep();
        eprintln!(
            "{}",
            terminal::text(&format!(
                "Starting Ollama at {endpoint}; startup log: {}",
                path.display()
            ))
        );
        Ok(server)
    }

    fn exited(&self) -> io::Result<bool> {
        // Keep the leader's PID reserved until all signals to its group are sent.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.id() as _,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        Err(error)
    }

    fn check_running(&self) -> Result<(), Error> {
        match self.exited() {
            Ok(false) => Ok(()),
            Ok(true) => Err(Error::Provider(
                "'ollama serve' exited before becoming ready. Inspect the startup log and check whether the port is already in use".into(),
            )),
            Err(e) => Err(Error::Provider(format!("Cannot check 'ollama serve': {e}"))),
        }
    }
}

impl Drop for OwnedServer {
    fn drop(&mut self) {
        let group = -(self.0.id() as i32);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        // Only signal while the child still reserves the process group ID.
        let mut exited = self.exited();
        if exited.is_ok() {
            unsafe { libc::kill(group, libc::SIGTERM) };
            while matches!(exited, Ok(false)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
                exited = self.exited();
            }
            if exited.is_ok() {
                unsafe { libc::kill(group, libc::SIGKILL) };
            }
        }
        let _ = self.0.wait();
    }
}

enum Probe {
    Ready(String),
    Refused,
    Invalid,
}

async fn probe(client: &Client, endpoint: &str) -> Probe {
    let mut response = match client.get(format!("{endpoint}/api/version")).send().await {
        Ok(response) => response,
        Err(error) => {
            if error.is_connect() {
                let mut source = error.source();
                while let Some(cause) = source {
                    if cause
                        .downcast_ref::<io::Error>()
                        .is_some_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
                    {
                        return Probe::Refused;
                    }
                    source = cause.source();
                }
            }
            return Probe::Invalid;
        }
    };
    if response.status() != StatusCode::OK {
        return Probe::Invalid;
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= VERSION_LIMIT => {
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => return Probe::Invalid,
        }
    }
    serde_json::from_slice::<Value>(&body)
        .ok()
        .filter(|value| value.get("error").is_none())
        .and_then(|value| {
            value["version"]
                .as_str()
                .filter(|version| !version.trim().is_empty())
                .map(str::to_owned)
        })
        .map_or(Probe::Invalid, Probe::Ready)
}

pub(super) async fn ensure_ready(
    endpoint: &str,
    startup_timeout: Duration,
    cancel: &Cancellation,
) -> Result<(String, Option<OwnedServer>), Error> {
    ensure_ready_with(
        endpoint,
        DEFAULT_OLLAMA_URL,
        startup_timeout,
        cancel,
        Command::new("ollama"),
    )
    .await
}

async fn ensure_ready_with(
    endpoint: &str,
    default_endpoint: &str,
    startup_timeout: Duration,
    cancel: &Cancellation,
    command: Command,
) -> Result<(String, Option<OwnedServer>), Error> {
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| Error::Provider(format!("Cannot prepare the Ollama readiness check: {e}")))?;
    let invalid = || {
        Error::Provider(format!(
            "No valid Ollama /api/version response at {endpoint}. Check for another service or an unresponsive Ollama server on that port"
        ))
    };
    tokio::select! { biased;
        _ = cancel.wait() => return Err(Error::Canceled),
        result = probe(&client, endpoint) => match result {
            Probe::Ready(version) => return Ok((version, None)),
            Probe::Invalid => return Err(invalid()),
            Probe::Refused if endpoint != default_endpoint => return Err(Error::Provider(format!(
                "Ollama unavailable at {endpoint}. Start the custom server manually. Automatic startup is available at {default_endpoint}"
            ))),
            Probe::Refused => {}
        }
    }
    if cancel.is_canceled() {
        return Err(Error::Canceled);
    }

    let server = OwnedServer::spawn(command, endpoint)?;
    let deadline = Instant::now() + startup_timeout;
    let readiness = async {
        loop {
            server.check_running()?;
            match probe(&client, endpoint).await {
                Probe::Ready(version) => {
                    server.check_running()?;
                    return Ok(version);
                }
                Probe::Invalid => return Err(invalid()),
                Probe::Refused => sleep(POLL_INTERVAL).await,
            }
        }
    };
    let version = tokio::select! { biased;
        _ = cancel.wait() => return Err(Error::Canceled),
        result = timeout_at(deadline, readiness) => result.map_err(|_| Error::Provider(format!(
            "Ollama did not become ready within {startup_timeout:?} at {endpoint}. Inspect the startup log"
        )))??,
    };
    Ok((version, Some(server)))
}

#[cfg(test)]
#[path = "ollama_server_tests.rs"]
mod tests;
