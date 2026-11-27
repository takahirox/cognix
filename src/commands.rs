//! Exact argv policy and macOS Seatbelt isolation. Never retry unsandboxed.
use crate::{
    Cancellation,
    action::{ActionPlan, SandboxMode, unexecuted},
    network::{CURL, Fetch},
    schema,
};
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
    "/Library/Developer/CommandLineTools",
    "/Applications/Xcode.app/Contents/Developer",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/opt",
    "/opt/homebrew/lib",
    "/opt/homebrew/bin",
];
const COMMAND_PATH: &str = "/usr/bin:/bin:/opt/homebrew/bin";

pub fn parse_command(text: &str) -> Result<Vec<String>, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("--allow-command needs JSON argv: {e}"))?;
    let argv = schema::argv(&v)?;
    resolve_executable(&argv[0]).map_err(|e| format!("--allow-command: {e}"))?;
    Ok(argv)
}

fn resolve_executable(program: &str) -> Result<(PathBuf, PathBuf, fs::Metadata), &'static str> {
    let path = Path::new(program);
    if path.is_absolute() {
        return installed_executable(path)
            .map(|(canonical, metadata)| (path.to_owned(), canonical, metadata))
            .ok_or("Absolute executable must resolve to a file in a documented system runtime directory");
    }
    if program.is_empty() || program.contains('/') || matches!(program, "." | "..") {
        return Err("Use a bare command name or an absolute executable path");
    }
    for dir in COMMAND_PATH.split(':') {
        let invocation = Path::new(dir).join(program);
        if let Some((canonical, metadata)) = installed_executable(&invocation) {
            return Ok((invocation, canonical, metadata));
        }
    }
    Err(
        "Command not found in /usr/bin, /bin or /opt/homebrew/bin. Use an installed command or an eligible absolute path",
    )
}

fn installed_executable(path: &Path) -> Option<(PathBuf, fs::Metadata)> {
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.symlink_metadata().ok()?;
    if metadata.is_file()
        && metadata.permissions().mode() & 0o111 != 0
        && RUNTIME_DIRS
            .iter()
            .filter_map(|s| Path::new(s).canonicalize().ok())
            .any(|dir| canonical.starts_with(dir))
    {
        Some((canonical, metadata))
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionPolicy {
    Sandbox(SandboxMode),
    FullAccess,
}

pub(crate) struct PreparedCommand {
    argv: Vec<String>,
    invocation: PathBuf,
    executable: PathBuf,
    metadata: fs::Metadata,
    root: PathBuf,
    policy: ExecutionPolicy,
    pub fetch: Option<Fetch>,
}

impl PreparedCommand {
    pub(crate) fn new(
        root: &Path,
        argv: Vec<String>,
        policy: ExecutionPolicy,
    ) -> Result<Self, String> {
        let program = argv.first().ok_or("Command needs an executable")?;
        let fetch = match policy {
            ExecutionPolicy::Sandbox(_) => {
                sandbox_available()?;
                Fetch::from_argv(&argv)?
            }
            ExecutionPolicy::FullAccess => None,
        };
        let (invocation, executable, metadata) =
            Self::resolve(root, policy, program, fetch.is_some())?;
        for path in [root, &invocation, &executable] {
            path.to_str().ok_or("Command paths must be UTF-8")?;
        }
        Ok(Self {
            argv,
            invocation,
            executable,
            metadata,
            root: root.to_owned(),
            policy,
            fetch,
        })
    }

    fn resolve(
        root: &Path,
        policy: ExecutionPolicy,
        program: &str,
        fetch: bool,
    ) -> Result<(PathBuf, PathBuf, fs::Metadata), String> {
        if let ExecutionPolicy::Sandbox(_) = policy {
            return resolve_executable(if fetch { CURL } else { program }).map_err(str::to_owned);
        }
        let path = Path::new(program);
        let mut paths = Vec::new();
        if path.is_absolute() || program.contains('/') {
            paths.push(root.join(path));
        } else if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path).map(|p| root.join(p).join(program)));
        }
        paths
            .into_iter()
            .find_map(|invocation| {
                let executable = invocation.canonicalize().ok()?;
                let metadata = executable.metadata().ok()?;
                (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                    .then_some((invocation, executable, metadata))
            })
            .ok_or_else(|| "Executable not found on PATH or at the given path".into())
    }

    pub(crate) fn plan(&self) -> ActionPlan {
        let working_directory = if self.fetch.is_some() {
            "private scratch".to_owned()
        } else {
            format!("{:?}", self.root)
        };
        let mut preview = format!(
            "argv = {}\nWorking directory = {working_directory}\n",
            json!(self.argv)
        );
        if self.invocation != self.executable && self.invocation.to_str() != Some(&self.argv[0]) {
            preview.push_str(&format!("Invocation path = {:?}\n", self.invocation));
        }
        if self.executable.to_str() != Some(&self.argv[0]) {
            preview.push_str(&format!("Resolved executable = {:?}\n", self.executable));
        }
        let restrictions = match self.policy {
            _ if self.fetch.is_some() => "Brokered HTTP(S) GET. Redirects are disabled.",
            ExecutionPolicy::Sandbox(SandboxMode::ReadOnly) => {
                "Read-only project; no network; scratch writes only."
            }
            ExecutionPolicy::Sandbox(SandboxMode::WorkspaceWrite) => {
                "Writes to the live project and private scratch; no network or .git access. Child processes inherit the sandbox."
            }
            ExecutionPolicy::FullAccess => {
                "Full access to files, network and inherited environment."
            }
        };
        preview.push_str(restrictions);
        let permissions = self.fetch.as_ref().map(Fetch::scope);
        if let Some(scope) = &permissions {
            preview.push_str(&format!("\nRequested network destination = {scope}"));
        }
        ActionPlan {
            kind: "command",
            path: None,
            preview,
            evidence: json!({"executable":self.executable,"invocation":self.invocation,
                "project_root":self.root, "working_directory":if self.fetch.is_some() { json!("private scratch") } else { json!(self.root) },
                "restrictions":restrictions,"requested_network":permissions,
                "effective_argv":self.fetch.as_ref().map(Fetch::args),
                "executable_identity":[self.metadata.dev(),self.metadata.ino(),self.metadata.ctime() as u64,self.metadata.ctime_nsec() as u64]}),
        }
    }

    pub(crate) fn low_risk(&self) -> bool {
        // Only fixed argument shapes for known system binaries. An absolute alias,
        // command option, filename or wrapper never qualifies for this exemption.
        matches!((self.executable.to_str(), self.argv.as_slice()),
            (Some("/bin/ls"), [name]) if name == "ls" || name == "/bin/ls")
            || matches!((self.executable.to_str(), self.argv.as_slice()),
            (Some("/bin/pwd"), [name]) if name == "pwd" || name == "/bin/pwd")
    }

    fn check_executable(&self) -> Result<(), String> {
        let changed = "Command executable changed after preview; command not run";
        let (invocation, resolved, current) =
            Self::resolve(&self.root, self.policy, &self.argv[0], self.fetch.is_some())
                .map_err(|_| changed)?;
        if invocation != self.invocation
            || resolved != self.executable
            || current.dev() != self.metadata.dev()
            || current.ino() != self.metadata.ino()
            || current.ctime() != self.metadata.ctime()
            || current.ctime_nsec() != self.metadata.ctime_nsec()
        {
            return Err(changed.into());
        }
        Ok(())
    }
}

fn sandbox_available() -> Result<(), String> {
    if !cfg!(target_os = "macos") || !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "Command sandbox unavailable: only macOS sandbox-exec is supported; command not run"
                .into(),
        );
    }
    Ok(())
}

pub fn sandbox_argv(root: &Path, scratch: &Path, argv: &[String]) -> Result<Vec<String>, String> {
    wrap_sandbox(root, scratch, argv, SandboxMode::ReadOnly)
}

fn wrap_sandbox(
    root: &Path,
    scratch: &Path,
    argv: &[String],
    mode: SandboxMode,
) -> Result<Vec<String>, String> {
    sandbox_available()?;
    let quote =
        |p: &Path| serde_json::to_string(p.to_str().expect("validated sandbox path")).unwrap();
    for path in [root, scratch] {
        path.to_str().ok_or("Sandbox paths must be UTF-8")?;
    }
    let readable: Vec<PathBuf> = std::iter::once(root.to_owned())
        .chain(std::iter::once(scratch.to_owned()))
        .chain(
            RUNTIME_DIRS
                .iter()
                .filter_map(|p| Path::new(p).canonicalize().ok())
                .filter(|p| p.to_str().is_some()),
        )
        .collect();
    let reads = readable
        .iter()
        .map(|p| format!("(subpath {})", quote(p)))
        .collect::<Vec<_>>()
        .join(" ");
    let workspace = match mode {
        SandboxMode::ReadOnly => String::new(),
        SandboxMode::WorkspaceWrite => format!(
            "(allow process-fork) (allow file-write* (subpath {})) (deny file-write* (literal {})) (deny file-link)",
            quote(root),
            quote(root)
        ),
    };
    let profile = format!(
        r#"(version 1) (deny default) (allow process-exec) (allow sysctl-read) (allow file-read-metadata) (allow file-read* {reads} (literal "/") (literal "/dev/null") (literal "/dev/urandom") (literal "/dev/random") (subpath "/private/var/db/dyld")) (allow file-write* (subpath {}) (literal "/dev/null")) {workspace} (deny file-read* file-write* (regex #"/[.][gG][iI][tT](/|$)"))"#,
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
    if cancel.is_canceled() {
        return Ok(unexecuted("canceled"));
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let prepared = PreparedCommand::new(
        &root,
        argv.to_vec(),
        ExecutionPolicy::Sandbox(SandboxMode::ReadOnly),
    )?;
    // This low-level API only runs sandboxed commands; it has no approval path.
    if prepared.fetch.is_some() {
        return Err("Brokered network requests require approval through the runtime".into());
    }
    run_prepared(prepared, cancel, timeout, limit)
}

pub(crate) fn run_prepared(
    prepared: PreparedCommand,
    cancel: &Cancellation,
    timeout: Duration,
    limit: usize,
) -> Result<Value, String> {
    let root = &prepared.root;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("Command timeout is too large")?;
    if cancel.is_canceled() {
        return Ok(unexecuted("canceled"));
    }
    let mut temporary = None;
    let mut command = Command::new(&prepared.invocation);
    command.current_dir(root).stdin(Stdio::null());
    if let ExecutionPolicy::Sandbox(mode) = prepared.policy {
        let scratch_parent = std::env::temp_dir()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if scratch_parent.starts_with(root) {
            return Err("Command scratch directory would be inside the project".into());
        }
        let temp = tempfile::Builder::new()
            .prefix("cognix-check-")
            .tempdir_in(scratch_parent)
            .map_err(|e| e.to_string())?;
        let scratch = temp.path().canonicalize().map_err(|e| e.to_string())?;
        let scratch_str = scratch.to_str().ok_or("Non-UTF8 scratch")?;
        if let Some(fetch) = &prepared.fetch {
            // Only the fixed system HTTP client gets network access. No proposed
            // flags, workspace cwd, proxy settings or user configuration cross
            // this boundary. The reviewed URL is the only outbound request.
            command.current_dir(&scratch).args(fetch.args());
        } else {
            let validation = validate_project_reads(root, cancel, deadline);
            if cancel.is_canceled() {
                return Ok(unexecuted("canceled"));
            }
            if Instant::now() >= deadline {
                return Ok(unexecuted("limit_reached"));
            }
            validation?;
            let mut argv = prepared.argv.clone();
            argv[0] = prepared
                .invocation
                .to_str()
                .ok_or("Non-UTF8 command path")?
                .into();
            let wrapped = wrap_sandbox(root, &scratch, &argv, mode)?;
            command = Command::new(&wrapped[0]);
            command
                .current_dir(root)
                .stdin(Stdio::null())
                .args(&wrapped[1..]);
        }
        command.env_clear().envs([
            ("PATH", COMMAND_PATH),
            ("HOME", scratch_str),
            ("TMPDIR", scratch_str),
            ("TMP", scratch_str),
            ("TEMP", scratch_str),
            ("LANG", "en_US.UTF-8"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("PYTHONNOUSERSITE", "1"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ]);
        temporary = Some(temp);
    } else {
        command.args(&prepared.argv[1..]);
    }
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
    // Refuse to launch if the submitted path no longer resolves to the approved executable.
    let executable_check = prepared.check_executable();
    if cancel.is_canceled() {
        return Ok(unexecuted("canceled"));
    }
    if Instant::now() >= deadline {
        return Ok(unexecuted("limit_reached"));
    }
    executable_check?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("Command launch failed: {e}"))?;
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
                "Launched command process {pid} could not be reaped: {e}"
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
    if prepared.fetch.is_none()
        && prepared.policy != ExecutionPolicy::Sandbox(SandboxMode::ReadOnly)
        && !prepared.low_risk()
    {
        result["workspace_mutations_possible"] = json!(true);
    }
    if let Some(error) = io_error {
        result["error"] = json!(error);
    }
    drop(temporary);
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
