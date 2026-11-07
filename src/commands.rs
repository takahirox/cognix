//! Exact argv policy and macOS Seatbelt isolation. Never retry unsandboxed.
use crate::{Cancellation, schema};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(test)]
thread_local! {
    pub(crate) static INTERRUPT_AT: std::cell::RefCell<Option<(&'static str,Cancellation)>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn boundary(phase: &str) {
    INTERRUPT_AT.with(|point| {
        if let Some((selected, cancel)) = &*point.borrow() {
            if *selected == phase {
                cancel.cancel();
            }
        }
    });
}

pub const RUNTIME_DIRS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/Library/Frameworks",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/opt",
    "/opt/homebrew/lib",
    "/opt/homebrew/bin",
];
pub fn parse_command(text: &str) -> Result<Vec<String>, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("--allow-command needs JSON argv: {e}"))?;
    let argv = schema::argv(&v)?;
    let p = Path::new(&argv[0]);
    let canonical = p
        .canonicalize()
        .map_err(|_| "Allowed commands need an existing absolute executable")?;
    if !p.is_absolute()
        || !canonical.is_file()
        || fs::metadata(&canonical)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        || !RUNTIME_DIRS
            .iter()
            .filter_map(|s| Path::new(s).canonicalize().ok())
            .any(|dir| canonical.starts_with(dir))
    {
        return Err("Allowed commands need an absolute executable installed in a documented system runtime directory".into());
    }
    Ok(argv)
}
pub fn sandbox_argv(root: &Path, scratch: &Path, argv: &[String]) -> Result<Vec<String>, String> {
    if !cfg!(target_os = "macos") || !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "Command sandbox unavailable: only macOS sandbox-exec is supported; command not run"
                .into(),
        );
    }
    let quote = |p: &Path| serde_json::to_string(&p.to_string_lossy()).unwrap();
    let readable: Vec<PathBuf> = std::iter::once(root.to_owned())
        .chain(std::iter::once(scratch.to_owned()))
        .chain(
            RUNTIME_DIRS
                .iter()
                .filter_map(|p| Path::new(p).canonicalize().ok()),
        )
        .collect();
    let reads = readable
        .iter()
        .map(|p| format!("(subpath {})", quote(p)))
        .collect::<Vec<_>>()
        .join(" ");
    let profile = format!(
        "(version 1) (deny default) (allow process-exec) (allow sysctl-read) (allow file-read-metadata) (allow file-read* {reads} (literal \"/\") (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/random\") (subpath \"/private/var/db/dyld\")) (allow file-write* (subpath {}) (literal \"/dev/null\")) (deny file-read* (regex #\"/[.][gG][iI][tT](/|$)\"))",
        quote(scratch)
    );
    let mut out = vec!["/usr/bin/sandbox-exec".into(), "-p".into(), profile];
    out.extend_from_slice(argv);
    Ok(out)
}

pub fn run(
    root: &Path,
    argv: &[String],
    cancel: &Cancellation,
    timeout: Duration,
    limit: usize,
) -> Result<Value, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("Command timeout is too large")?;
    if cancel.is_canceled() {
        return Ok(json!({"status":"canceled","executed":false}));
    }
    let temporary = tempfile::Builder::new()
        .prefix("cognix-check-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let scratch = temporary.path().canonicalize().map_err(|e| e.to_string())?;
    let wrapped = sandbox_argv(root, &scratch, argv)?;
    let mut command = Command::new(&wrapped[0]);
    command
        .args(&wrapped[1..])
        .current_dir(root)
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin:/opt/homebrew/bin"),
            ("HOME", scratch.to_str().ok_or("Non-UTF8 scratch")?),
            ("TMPDIR", scratch.to_str().unwrap()),
            ("TMP", scratch.to_str().unwrap()),
            ("TEMP", scratch.to_str().unwrap()),
            ("LANG", "en_US.UTF-8"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONNOUSERSITE", "1"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ])
        .stdin(Stdio::null());
    // One pipe keeps stdout/stderr ordering and permits a single combined byte cap.
    let (mut reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
    command
        .stdout(writer.try_clone().map_err(|e| e.to_string())?)
        .stderr(writer);
    // SAFETY: setsid is async-signal-safe; no locks/allocations after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if cancel.is_canceled() {
        return Ok(json!({"status":"canceled","executed":false}));
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Sandbox launch failed; no unsandboxed retry: {e}"))?;
    #[cfg(test)]
    boundary("spawned");
    drop(command);
    // A signal handler only sets an atomic flag: spawn ownership, commit and ledger
    // updates cannot be unwound by SIGINT, even at the return from spawn/rename.
    let pid = child.id() as i32;
    let fd = reader.as_raw_fd();
    // SAFETY: fd belongs to reader and remains open throughout the loop.
    let nonblocking = unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
    let mut output = Vec::new();
    let mut state = None;
    let mut io_error = None;
    let mut eof = false;
    if nonblocking < 0 {
        io_error = Some(std::io::Error::last_os_error().to_string());
    }
    while io_error.is_none() {
        if cancel.is_canceled() {
            state = Some("canceled");
            break;
        }
        if Instant::now() >= deadline {
            state = Some("limit_reached");
            break;
        }
        let mut buf = [0u8; 4096];
        match reader.read(&mut buf) {
            Ok(0) => eof = true,
            Ok(n) => {
                let remaining = limit.saturating_sub(output.len());
                output.extend_from_slice(&buf[..n.min(remaining)]);
                if n > remaining {
                    state = Some("limit_reached");
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => {
                io_error = Some(e.to_string());
                break;
            }
        }
        match child.try_wait() {
            Ok(Some(_)) if eof => break,
            Ok(_) => {}
            Err(e) => {
                io_error = Some(e.to_string());
                break;
            }
        }
        if eof {
            std::thread::sleep(Duration::from_millis(10));
        } else {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd with a live descriptor.
            unsafe {
                libc::poll(&mut poll, 1, 10);
            }
        }
    }
    // Always reap; no cancellation check may bypass cleanup or lose launch evidence.
    #[cfg(test)]
    boundary("cleanup");
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let exit = match child.wait() {
        Ok(exit) => Some(exit),
        Err(e) => {
            io_error = Some(format!(
                "Launched sandbox process {pid} could not be reaped: {e}"
            ));
            None
        }
    };
    #[cfg(test)]
    boundary("reaped");
    if cancel.is_canceled() {
        state = Some("canceled");
    }
    let code = exit.map(|e| e.code().unwrap_or_else(|| -e.signal().unwrap_or(1)));
    // Lossy decoding can expand invalid bytes; keep even the decoded text bounded.
    let mut decoded = String::from_utf8_lossy(&output).into_owned();
    if decoded.len() > limit {
        let mut end = limit;
        while !decoded.is_char_boundary(end) {
            end -= 1;
        }
        decoded.truncate(end);
    }
    let mut result = json!({"status":state.unwrap_or(if io_error.is_some(){"failed"}else if exit.is_some_and(|e|e.success()){"passed"}else{"failed"}),"executed":true,"pid":pid,"exit_code":code,"output":decoded,"limit":if state==Some("limit_reached"){Some("time or output")}else{None}});
    if let Some(error) = io_error {
        result["error"] = json!(error);
    }
    Ok(result)
}
