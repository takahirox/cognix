//! Exact argv policy and macOS Seatbelt isolation. Never retry unsandboxed.
use crate::{Cancellation, schema};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
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

fn validate_project_reads(
    root: &Path,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<(), String> {
    // Seatbelt's subpath rules cannot distinguish an ordinary project file from
    // a hard link to outside data or .git. Reject the entire command rather than
    // trying to infer which paths an interpreter or test runner might open.
    // Run this after approval, immediately before launch. As with the file tools,
    // concurrent hostile changes by another same-user process are out of scope.
    let mut directories = vec![root.to_owned()];
    let mut entries = 0;
    while let Some(directory) = directories.pop() {
        let listing = fs::read_dir(directory).map_err(|e| {
            format!("Cannot inspect project command boundary; command not run: {e}")
        })?;
        for entry in listing {
            if cancel.is_canceled() || Instant::now() >= deadline {
                return Err("Project command boundary check interrupted".into());
            }
            entries += 1;
            if entries > 100_000 {
                return Err(
                    "Project command boundary exceeds 100000 entries; command not run".into(),
                );
            }
            let entry = entry.map_err(|e| format!("Cannot inspect project entry: {e}"))?;
            // Seatbelt already denies .git, including case aliases. Never follow
            // symlinks; Seatbelt checks their resolved targets at access time.
            if entry
                .file_name()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
            {
                continue;
            }
            let info = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("Cannot inspect project entry: {e}"))?;
            if info.is_file() && info.nlink() != 1 {
                return Err("Project contains a file with multiple hard links; command not run. Use an ordinary copy of the file instead".into());
            }
            if info.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn descriptor_buffer() -> std::io::Result<Vec<libc::proc_fdinfo>> {
    // Allocate before fork. The kernel includes spare entries in its size estimate;
    // if other threads outgrow it before fork, the child fails closed below.
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if bytes <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((0..bytes as usize / size_of::<libc::proc_fdinfo>())
        .map(|_| libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        })
        .collect())
}

#[cfg(target_os = "macos")]
fn sanitize_descriptors(buffer: &mut [libc::proc_fdinfo]) -> std::io::Result<()> {
    // Only called in the forked child. libproc's proc_pidinfo is a thin syscall
    // wrapper, so this uses no allocator or userspace locks after fork.
    // Enumerate the child's actual table, including Rust's newly created spawn
    // error pipe and descriptors above any subsequently lowered RLIMIT_NOFILE.
    let capacity = i32::try_from(std::mem::size_of_val(buffer))
        .map_err(|_| std::io::Error::from_raw_os_error(libc::E2BIG))?;
    if capacity == 0 {
        return Err(std::io::Error::from_raw_os_error(libc::E2BIG));
    }
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            buffer.as_mut_ptr().cast(),
            capacity,
        )
    };
    if bytes <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    // A full buffer may be truncated. Never launch with an incomplete list.
    if bytes >= capacity || !(bytes as usize).is_multiple_of(size_of::<libc::proc_fdinfo>()) {
        return Err(std::io::Error::from_raw_os_error(libc::E2BIG));
    }
    for entry in &buffer[..bytes as usize / size_of::<libc::proc_fdinfo>()] {
        if entry.proc_fd >= 3 {
            let flags = unsafe { libc::fcntl(entry.proc_fd, libc::F_GETFD) };
            if flags < 0
                || (flags & libc::FD_CLOEXEC == 0
                    && unsafe {
                        libc::fcntl(entry.proc_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC)
                    } < 0)
            {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    // CLOEXEC, rather than close(), keeps Rust's error pipe usable if exec fails.
    // The caller's stdin/stdout/stderr have already been redirected by Command.
    Ok(())
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
    let validation = validate_project_reads(root, cancel, deadline);
    if cancel.is_canceled() {
        return Ok(json!({"status":"canceled","executed":false}));
    }
    if Instant::now() >= deadline {
        return Ok(json!({"status":"limit_reached","executed":false}));
    }
    validation?;
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
    #[cfg(target_os = "macos")]
    let mut descriptors = descriptor_buffer()
        .map_err(|e| format!("Cannot prepare descriptor isolation; command not run: {e}"))?;
    // SAFETY: only syscall wrappers and preallocated memory are used after fork.
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "macos")]
            sanitize_descriptors(&mut descriptors)?;
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

#[cfg(all(test, target_os = "macos"))]
mod descriptor_tests {
    use super::*;

    #[test]
    fn descriptor_sanitization_fails_closed_and_preserves_spawn_errors() {
        // An undersized list must abort spawn; it cannot silently omit handles.
        let mut small = descriptor_buffer().unwrap();
        small.truncate(1);
        let mut command = Command::new("/usr/bin/true");
        unsafe {
            command.pre_exec(move || sanitize_descriptors(&mut small));
        }
        assert_eq!(
            command.spawn().unwrap_err().raw_os_error(),
            Some(libc::E2BIG)
        );

        // A failed exec after sanitization still reaches the parent's spawn pipe.
        let root = tempfile::tempdir().unwrap();
        let mut descriptors = descriptor_buffer().unwrap();
        let mut command = Command::new(root.path().join("missing-executable"));
        unsafe {
            command.pre_exec(move || sanitize_descriptors(&mut descriptors));
        }
        assert_eq!(
            command.spawn().unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
    }
}
