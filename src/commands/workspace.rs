//! macOS workspace command lifetime, independent of process groups and sessions.
use crate::Cancellation;
use serde_json::json;
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::Path,
    process::{Command, ExitStatus},
    ptr, thread,
    time::{Duration, Instant},
};

const LAUNCHCTL: &str = "/bin/launchctl";
const PID_IDENTITY: i32 = 17;
const PID_COALITION: i32 = 20;
const START: &[u8] = b"start\n";

// Keep the launcher live and in its own coalition until the parent has verified
// containment and attached an exit watcher. No project code runs at this point.
const LAUNCHER: &str = r#"
cd -- "$1" || exit 125
printf '%s\n' "$$" > "$2" || exit 125
IFS= read -r permit < "$3" && [ "$permit" = start ] || exit 125
shift 3
exec "$@"
"#;

#[repr(C)]
struct ProcessIdentity {
    _uuid: [u8; 16],
    _unique_id: u64,
    _parent_unique_id: u64,
    pid_version: u32,
    _original_parent_pid_version: u32,
    _reserved: [u64; 2],
}

unsafe extern "C" {
    fn coalition_info_resource_usage(id: u64, buffer: *mut u64, size: usize) -> i32;
}

type Signal = unsafe extern "C" fn(token: *mut [u32; 8], signal: i32) -> i32;

struct Service {
    target: Option<String>,
    _directory: tempfile::TempDir,
}

impl Service {
    fn unload(&mut self) -> io::Result<()> {
        if let Some(target) = &self.target {
            checked(Command::new(LAUNCHCTL).args(["bootout", target]))?;
            self.target = None;
        }
        Ok(())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.unload();
    }
}

pub(super) struct Job {
    pid: i32,
    coalition: u64,
    permit: File,
    watcher: OwnedFd,
    signal: Signal,
    exit: Option<ExitStatus>,
    service: Service,
}

impl Job {
    pub(super) fn prepare(
        command: &Command,
        scratch_parent: &Path,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> io::Result<Option<(File, Self)>> {
        // Older macOS versions lack PID-version signaling. Leave the other
        // command profiles usable and refuse this one before running anything.
        let symbol =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"proc_signal_with_audittoken".as_ptr()) };
        if symbol.is_null() {
            return Err(io::Error::other(
                "macOS lacks identity-bound process signaling",
            ));
        }
        let signal = unsafe { std::mem::transmute::<*mut libc::c_void, Signal>(symbol) };
        let directory = tempfile::Builder::new()
            .prefix("cognix-job-")
            .tempdir_in(scratch_parent)?;
        let path = directory.path();
        let label = format!("org.cognix.{}", path.file_name().unwrap().to_string_lossy());
        let uid = unsafe { libc::geteuid() };
        let domain = if uid == 0 {
            "system".to_owned()
        } else {
            format!("user/{uid}")
        };
        let output = path.join("output");
        let ready = path.join("pid");
        let go = path.join("permit");
        let reader = open_fifo(&output, false)?;
        let permit = open_fifo(&go, true)?;

        let mut argv = vec![
            "/usr/bin/env".to_owned(),
            "-i".into(),
            "/bin/sh".into(),
            "-c".into(),
            LAUNCHER.into(),
            "cognix-workspace".into(),
        ];
        for path in [command.get_current_dir().unwrap(), &ready, &go] {
            argv.push(path.to_str().ok_or_else(invalid_path)?.into());
        }
        argv.extend(["/usr/bin/env".into(), "-i".into()]);
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                argv.push(format!(
                    "{}={}",
                    key.to_str().ok_or_else(invalid_path)?,
                    value.to_str().ok_or_else(invalid_path)?
                ));
            }
        }
        for arg in std::iter::once(command.get_program()).chain(command.get_args()) {
            argv.push(arg.to_str().ok_or_else(invalid_path)?.into());
        }
        let mut config = json!({"Label":label,"ProgramArguments":argv,"RunAtLoad":true,
            "StandardInPath":"/dev/null","StandardOutPath":output,"StandardErrorPath":output});
        if uid != 0 {
            config["LimitLoadToSessionType"] = json!("Background");
        }
        let source = path.join("job.json");
        let plist = path.join("job.plist");
        fs::write(&source, config.to_string())?;
        checked(
            Command::new("/usr/bin/plutil")
                .args(["-convert", "binary1", "-o"])
                .arg(&plist)
                .arg(&source),
        )?;
        if cancel.is_canceled() || Instant::now() >= deadline {
            return Ok(None);
        }
        let service = Service {
            target: Some(format!("{domain}/{label}")),
            _directory: directory,
        };
        checked(
            Command::new(LAUNCHCTL)
                .args(["bootstrap", &domain])
                .arg(plist),
        )?;
        let pid = loop {
            if cancel.is_canceled() || Instant::now() >= deadline {
                return Ok(None);
            }
            match fs::read_to_string(ready.as_path()) {
                Ok(text) => {
                    if let Some(pid) = text.strip_suffix('\n') {
                        let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 1);
                        break pid
                            .ok_or_else(|| io::Error::other("Invalid workspace launcher PID"))?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(5));
        };
        let coalition = process_coalition(pid)?;
        if coalition <= 1
            || coalition == process_coalition(unsafe { libc::getpid() })?
            || active_tasks(coalition)? != 1
        {
            return Err(io::Error::other(
                "Workspace launcher has no dedicated resource coalition",
            ));
        }
        // Also probe identity-based signaling before allowing any project code.
        signal_process(
            signal,
            pid,
            process_identity(pid)?.pid_version,
            libc::SIGCONT,
        )?;
        let watcher = watch_exit(pid)?;
        Ok(Some((
            reader,
            Self {
                pid,
                coalition,
                permit,
                watcher,
                signal,
                exit: None,
                service,
            },
        )))
    }

    pub(super) fn start(&mut self) -> io::Result<()> {
        self.permit.write_all(START)
    }

    pub(super) fn id(&self) -> i32 {
        self.pid
    }

    pub(super) fn exit_status(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_none() {
            let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
            let no_wait = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let count = unsafe {
                libc::kevent(
                    self.watcher.as_raw_fd(),
                    ptr::null(),
                    0,
                    event.as_mut_ptr(),
                    1,
                    &no_wait,
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            } else if count == 1 {
                let event = unsafe { event.assume_init() };
                if event.fflags & libc::NOTE_EXITSTATUS == 0 {
                    return Err(io::Error::other(
                        "Workspace command exit status was unavailable",
                    ));
                }
                self.exit = Some(ExitStatus::from_raw(event.data as i32));
            }
        }
        Ok(self.exit)
    }

    pub(super) fn stop(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            // The kernel count includes detached and reparented descendants, as
            // well as children that raced our process list. Only zero proves drain.
            let remaining = active_tasks(self.coalition);
            if matches!(remaining, Ok(0)) && self.exit_status()?.is_some() {
                return self.service.unload();
            }
            let signaled = signal_members(self.signal, self.coalition);
            if Instant::now() >= deadline {
                let detail = remaining.err().or_else(|| signaled.err());
                let message = "Could not verify workspace command descendants stopped";
                return Err(io::Error::other(match detail {
                    Some(error) => format!("{message}: {error}"),
                    None => message.into(),
                }));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn invalid_path() -> io::Error {
    io::Error::other("Workspace launcher requires UTF-8 arguments and paths")
}

fn open_fifo(path: &Path, writable: bool) -> io::Result<File> {
    let name = CString::new(path.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } < 0 {
        return Err(io::Error::last_os_error());
    }
    OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

fn checked(command: &mut Command) -> io::Result<()> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{}: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn watch_exit(pid: i32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::kqueue() };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let watcher = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let event = libc::kevent {
        ident: pid as _,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
        data: 0,
        udata: ptr::null_mut(),
    };
    if unsafe { libc::kevent(fd, &event, 1, ptr::null_mut(), 0, ptr::null()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(watcher)
}

fn process_info<T>(pid: i32, flavor: i32) -> io::Result<T> {
    let mut info = std::mem::MaybeUninit::<T>::uninit();
    let size = size_of::<T>() as i32;
    let bytes = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
    if bytes != size {
        return Err(io::Error::last_os_error());
    }
    // Only called with the integer layouts for the two flavors above.
    Ok(unsafe { info.assume_init() })
}

fn process_coalition(pid: i32) -> io::Result<u64> {
    let info: [u64; 5] = process_info(pid, PID_COALITION)?;
    Ok(info[0])
}

fn process_identity(pid: i32) -> io::Result<ProcessIdentity> {
    process_info(pid, PID_IDENTITY)
}

fn active_tasks(coalition: u64) -> io::Result<u64> {
    // XNU's coalition usage ABI starts with the started and exited counters.
    // Reserve space for the rest of the structure and newer trailing fields.
    let mut usage = [0u64; 128];
    if unsafe { coalition_info_resource_usage(coalition, usage.as_mut_ptr(), size_of_val(&usage)) }
        != 0
    {
        return Err(io::Error::last_os_error());
    }
    usage[0]
        .checked_sub(usage[1])
        .ok_or_else(|| io::Error::other("Invalid resource coalition count"))
}

fn signal_process(signal: Signal, pid: i32, version: u32, value: i32) -> io::Result<()> {
    // XNU validates both PID and version atomically before signaling.
    let mut token = [0, 0, 0, 0, 0, pid as u32, 0, version];
    let error = unsafe { signal(&mut token, value) };
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

fn signal_members(signal: Signal, coalition: u64) -> io::Result<()> {
    let estimate = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    if estimate <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pids = vec![0i32; estimate as usize + 64];
    let count =
        unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size_of_val(&pids[..]) as i32) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    for &pid in &pids[..count as usize] {
        if pid <= 1 || process_coalition(pid).ok() != Some(coalition) {
            continue;
        }
        let Ok(identity) = process_identity(pid) else {
            continue;
        };
        if process_coalition(pid).ok() == Some(coalition) {
            // An exec or exit can invalidate the token. The next pass will
            // cover anything still alive, up to the cleanup deadline.
            let _ = signal_process(signal, pid, identity.pid_version, libc::SIGKILL);
        }
    }
    Ok(())
}
