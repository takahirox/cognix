use crate::{
    Cancellation,
    action::{ActionPlan, finish_result, unexecuted},
    commands::{self, ExecutionPolicy, PreparedCommand},
    network::{Broker, Fetch, MAX_BROKER_BODY, Request},
    review::{Decision, Reviewer},
    schema,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::CString,
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

// Internal milestone-2 preparation only; deliberately absent from tool dispatch.
#[allow(dead_code)]
#[path = "patch.rs"]
pub(crate) mod patch;

pub use crate::action::{Approval, ApprovalMode, SandboxMode};
#[cfg(test)]
type CommitHook = Box<dyn FnMut(&str)>;
pub struct Runtime {
    pub root: PathBuf,
    root_fd: File,
    pub approve: Approval,
    pub approval_mode: ApprovalMode,
    pub sandbox_mode: SandboxMode,
    pub reviewer: Option<Reviewer>,
    pub denied_network_origins: Vec<String>,
    human_decisions: Vec<Value>,
    denied_network_in_session: BTreeSet<String>,
    auto_denied: bool,
    pub cancel: Cancellation,
    /// Nonempty lists restrict proposals to exact argv matches.
    pub allowed_commands: Vec<Vec<String>>,
    pub timeout: Duration,
    pub output_limit: usize,
    seen: HashMap<String, String>,
    patch_reads: HashMap<String, patch::ReadEvidence>,
    pub modified: BTreeSet<String>,
    pub events: Vec<Value>,
    #[cfg(test)]
    pub hook: Option<CommitHook>,
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn cstring(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(err)
}
fn openat(parent: &File, name: &str, flags: i32, mode: u32) -> Result<File, String> {
    let name = cstring(name)?;
    // SAFETY: live parent descriptor and NUL-terminated name; returned fd is owned once.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(err(std::io::Error::last_os_error()))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn identity(a: &Metadata, b: &Metadata) -> bool {
    a.ino() == b.ino() && a.dev() == b.dev()
}
const STALE_PATCH: &str =
    "File changed after preview. Prepare a new patch against the current file";

struct PatchSource {
    file: File,
    bytes: Vec<u8>,
    info: Metadata,
}

struct PreparedPatch {
    path: String,
    parent: File,
    target: CString,
    before: Option<PatchSource>,
    after: String,
}

enum PreparedAction {
    Patch(PreparedPatch),
    Command(PreparedCommand),
}

impl PreparedAction {
    fn origin(&self) -> Option<String> {
        match self {
            Self::Command(command) => command.fetch.as_ref().map(Fetch::origin),
            Self::Patch(_) => None,
        }
    }
}

struct BrokerJob {
    broker: Broker,
    worker: Option<JoinHandle<Result<Value, String>>>,
    cancel: Cancellation,
    network_lifetime: Cancellation,
    deadline: Instant,
    requests: Vec<Value>,
}

impl BrokerJob {
    fn active(&self) -> bool {
        !self.network_lifetime.is_canceled() && Instant::now() < self.deadline
    }

    fn finish(mut self) -> Result<Value, String> {
        let mut result = self
            .worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| "Command worker failed")??;
        result["network_requests"] = json!(std::mem::take(&mut self.requests));
        Ok(result)
    }
}

impl Drop for BrokerJob {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.cancel.cancel();
            let _ = worker.join();
        }
    }
}

enum PatchPublication {
    Applied,
    RecoveryRequired,
}

// Own both the inode and its staging name so cleanup can check identity.
struct PatchTemporary<'a> {
    parent: &'a File,
    name: CString,
    file: File,
    preserve: bool,
}
impl PatchTemporary<'_> {
    fn owns_name(&self) -> bool {
        self.file_at(&self.name)
    }

    fn file_at(&self, name: &CString) -> bool {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        let Ok(info) = self.file.metadata() else {
            return false;
        };
        // SAFETY: live descriptors/name and valid stat output storage.
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return false;
        }
        let st = unsafe { st.assume_init() };
        // dev_t is signed on macOS and u64 on Linux.
        #[allow(clippy::unnecessary_cast)]
        let device = st.st_dev as u64;
        device == info.dev() && st.st_ino == info.ino()
    }

    fn exchange(&self, target: &CString) -> std::io::Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let fd = self.parent.as_raw_fd();
            #[cfg(target_os = "macos")]
            let (rename, flag) = (libc::renameatx_np, libc::RENAME_SWAP);
            #[cfg(target_os = "linux")]
            let (rename, flag) = (libc::renameat2, libc::RENAME_EXCHANGE);
            // SAFETY: both names are NUL-terminated and relative to a live directory fd.
            let result = unsafe { rename(fd, self.name.as_ptr(), fd, target.as_ptr(), flag) };
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("Atomic patch update is unavailable for {target:?}"),
        ))
    }
}
impl Drop for PatchTemporary<'_> {
    fn drop(&mut self) {
        if !self.preserve && self.owns_name() {
            unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }
}

impl Runtime {
    pub fn new(root: &Path, approve: Approval, cancel: Cancellation) -> Result<Self, String> {
        let root = root.canonicalize().map_err(err)?;
        root.to_str().ok_or("Project root must have a UTF-8 path")?;
        use std::os::unix::fs::OpenOptionsExt;
        let root_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)
            .map_err(err)?;
        Ok(Self {
            root,
            root_fd,
            approve,
            approval_mode: ApprovalMode::default(),
            sandbox_mode: SandboxMode::default(),
            reviewer: None,
            denied_network_origins: vec![],
            human_decisions: vec![],
            denied_network_in_session: BTreeSet::new(),
            auto_denied: false,
            cancel,
            allowed_commands: vec![],
            timeout: Duration::from_secs(30),
            output_limit: 16384,
            seen: HashMap::new(),
            patch_reads: HashMap::new(),
            modified: BTreeSet::new(),
            events: vec![],
            #[cfg(test)]
            hook: None,
        })
    }
    fn parts(path: &str) -> Result<Vec<&str>, String> {
        if path.is_empty() || path.contains('\0') || Path::new(path).is_absolute() {
            return Err("Use a relative project path".into());
        }
        if path
            .split('/')
            .any(|p| p == ".." || p.eq_ignore_ascii_case(".git"))
        {
            return Err("Parent traversal and .git access are denied".into());
        }
        let parts: Vec<_> = path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        if parts.is_empty() || matches!(path.rsplit('/').next(), Some("" | ".")) {
            return Err("Expected a file path".into());
        }
        Ok(parts)
    }
    fn directory(&self, parts: &[&str]) -> Result<File, String> {
        let mut fd = self.root_fd.try_clone().map_err(err)?;
        for part in parts {
            fd = openat(&fd, part, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        }
        Ok(fd)
    }
    fn read(parent: &File, name: &str) -> Result<PatchSource, String> {
        let file = openat(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let info = file.metadata().map_err(err)?;
        if !info.is_file() || info.nlink() != 1 {
            return Err("Only regular files with one hard link are supported".into());
        }
        if info.len() > schema::PATCH_FILE_BYTES as u64 {
            return Err("File exceeds 64 KiB".into());
        }
        let mut raw = Vec::new();
        (&file)
            .take(schema::PATCH_FILE_BYTES as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(err)?;
        if raw.len() > schema::PATCH_FILE_BYTES {
            return Err("File exceeds 64 KiB".into());
        }
        if patch::Fingerprint::new(&file.metadata().map_err(err)?) != patch::Fingerprint::new(&info)
        {
            return Err("File changed while reading".into());
        }
        Ok(PatchSource {
            file,
            bytes: raw,
            info,
        })
    }
    #[cfg(test)]
    fn observe_patch_source(&mut self, path: &str) -> Result<String, String> {
        let parts = Self::parts(path)?;
        let (name, parent_parts) = parts.split_last().unwrap();
        let parent = self.directory(parent_parts)?;
        let source = Self::read(&parent, name)?;
        let text = std::str::from_utf8(&source.bytes).map_err(err)?;
        if text.contains('\0') {
            return Err("Binary files are unsupported".into());
        }
        let digest = hash(&source.bytes);
        self.patch_reads.insert(
            parts.join("/"),
            patch::ReadEvidence::new(&parent, name, &source.info, &digest),
        );
        self.seen.insert(parts.join("/"), digest.clone());
        Ok(digest)
    }
    fn patch_parent_unchanged(&self, parts: &[&str], parent: &File) -> Result<(), String> {
        let root = self.root.symlink_metadata().map_err(err)?;
        if !root.is_dir() || !identity(&root, &self.root_fd.metadata().map_err(err)?) {
            return Err("Project root changed during session".into());
        }
        let fresh = self.directory(parts)?;
        if !identity(
            &fresh.metadata().map_err(err)?,
            &parent.metadata().map_err(err)?,
        ) {
            return Err("Parent directory changed during patch".into());
        }
        Ok(())
    }
    fn destination_absent(parent: &File, name: &CString) -> Result<(), String> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            return Err("Destination already exists; creation never overwrites".into());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(err(error));
        }
        Ok(())
    }
    fn check_patch_target(
        parent: &File,
        name: &str,
        target: &CString,
        before: Option<&PatchSource>,
    ) -> Result<(), String> {
        let Some(before) = before else {
            return Self::destination_absent(parent, target);
        };
        let current = Self::read(parent, name).map_err(|_| STALE_PATCH)?;
        if current.bytes != before.bytes
            || patch::Fingerprint::new(&current.info) != patch::Fingerprint::new(&before.info)
        {
            return Err(STALE_PATCH.into());
        }
        Ok(())
    }
    fn publish_update(
        &mut self,
        temporary: &mut PatchTemporary<'_>,
        staging: &str,
        target: &CString,
        before: &PatchSource,
    ) -> Result<PatchPublication, String> {
        let staged_info = temporary.file.metadata().map_err(err)?;
        // Keep the entry found at the target by the syscall until it has been verified.
        temporary.exchange(target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                STALE_PATCH.into()
            } else {
                err(error)
            }
        })?;
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("after_exchange");
        }
        let staged_fingerprint = patch::Fingerprint::new(&staged_info);
        let staged_unchanged = |temporary: &PatchTemporary<'_>| {
            temporary.file.metadata().is_ok_and(|info| {
                patch::Fingerprint::new(&info).matches_after_rename(&staged_fingerprint)
            })
        };
        let displaced = Self::read(temporary.parent, staging).ok().filter(|source| {
            source.bytes == before.bytes
                && patch::Fingerprint::new(&source.info)
                    .matches_after_rename(&patch::Fingerprint::new(&before.info))
                && temporary.file_at(target)
                && staged_unchanged(temporary)
        });
        if let Some(displaced) = displaced {
            // The preview keeps the original inode open, so it cannot be recycled.
            // Cleanup now owns the verified original instead of the published file.
            temporary.file = displaced.file;
            return Ok(PatchPublication::Applied);
        }

        // Restore only while the target still looks like our output. Even if a
        // second save races the swap back, exchange retains it at the staging name.
        if temporary.file_at(target)
            && staged_unchanged(temporary)
            && temporary.exchange(target).is_ok()
            && temporary.owns_name()
            && staged_unchanged(temporary)
        {
            return Err(STALE_PATCH.into());
        }
        temporary.preserve = true;
        Ok(PatchPublication::RecoveryRequired)
    }
    fn record_patch_effect(&mut self, path: &str) {
        self.modified.insert(path.to_owned());
        self.seen.remove(path);
        self.patch_reads.remove(path);
    }
    fn prepare_patch(&self, args: &Value) -> Result<(PreparedAction, ActionPlan), String> {
        let parts = Self::parts(args["path"].as_str().unwrap())?;
        let normalized = parts.join("/");
        let (name, parent_parts) = parts.split_last().unwrap();
        let parent = self.directory(parent_parts)?;
        let target = cstring(name)?;
        let new = args["new_text"].as_str().unwrap();
        self.patch_parent_unchanged(parent_parts, &parent)?;

        let (before, after, plan) = if let Some(old) = args["old_text"].as_str() {
            if old == new {
                return Err("Patch must change the file content".into());
            }
            let snapshot = Self::read(&parent, name)?;
            let text = std::str::from_utf8(&snapshot.bytes).map_err(err)?;
            if text.contains('\0') {
                return Err("Binary files are unsupported".into());
            }
            let mut after = text.to_owned();
            if old.is_empty() && text.is_empty() {
                after.push_str(new);
            } else {
                let mismatch =
                    "old_text must match exactly once (use empty text only for an empty file)";
                let first = old.chars().next().ok_or(mismatch)?;
                let start = text.find(old).ok_or(mismatch)?;
                if text[start + first.len_utf8()..].contains(old) {
                    return Err(mismatch.into());
                }
                after.replace_range(start..start + old.len(), new);
            }
            if after.len() > schema::PATCH_FILE_BYTES {
                return Err("Patched file exceeds 64 KiB".into());
            }
            let mut plan = ActionPlan::patch(&normalized, text, &after, old, new);
            plan.evidence["mode"] = json!(snapshot.info.mode() & 0o777);
            plan.evidence["after_sha256"] = json!(hash(after.as_bytes()));
            (Some(snapshot), after, plan)
        } else {
            Self::destination_absent(&parent, &target)?;
            (None, new.to_owned(), ActionPlan::creation(&normalized, new))
        };
        Ok((
            PreparedAction::Patch(PreparedPatch {
                path: normalized,
                parent,
                target,
                before,
                after,
            }),
            plan,
        ))
    }

    fn commit_patch(&mut self, patch: PreparedPatch) -> Result<Value, String> {
        let PreparedPatch {
            path: normalized,
            parent,
            target,
            before,
            after,
        } = patch;
        let parts = Self::parts(&normalized)?;
        let (name, parent_parts) = parts.split_last().unwrap();
        self.patch_parent_unchanged(parent_parts, &parent)?;
        Self::check_patch_target(&parent, name, &target, before.as_ref())?;

        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let staging = format!(
            ".cognix-patch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let file = openat(
            &parent,
            &staging,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let mut temporary = PatchTemporary {
            parent: &parent,
            name: cstring(&staging)?,
            file,
            preserve: false,
        };
        temporary.file.write_all(after.as_bytes()).map_err(err)?;
        if let Some(source) = &before {
            temporary
                .file
                .set_permissions(std::fs::Permissions::from_mode(source.info.mode() & 0o777))
                .map_err(err)?;
        }
        temporary.file.sync_all().map_err(err)?;
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("before_commit");
        }
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","path":normalized,"executed":false}));
        }
        self.patch_parent_unchanged(parent_parts, &parent)?;
        if !temporary.owns_name() || temporary.file.metadata().map_err(err)?.nlink() != 1 {
            return Err("Patch staging file changed before commit".into());
        }
        Self::check_patch_target(&parent, name, &target, before.as_ref())?;
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","path":normalized,"executed":false}));
        }
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("before_publish");
        }

        let publication = if let Some(before) = &before {
            self.publish_update(&mut temporary, &staging, &target, before)?
        } else {
            let fd = parent.as_raw_fd();
            // linkat enforces destination absence, including dangling links.
            if unsafe { libc::linkat(fd, temporary.name.as_ptr(), fd, target.as_ptr(), 0) } < 0 {
                return Err(err(std::io::Error::last_os_error()));
            }
            PatchPublication::Applied
        };
        // Own the evidence before cleanup or reporting cancellation.
        self.record_patch_effect(&normalized);
        if let PatchPublication::RecoveryRequired = publication {
            // Both names were affected by the exchange and require inspection.
            let recovery_path = Path::new(&normalized).with_file_name(&staging);
            let recovery_path = recovery_path.to_str().unwrap();
            self.record_patch_effect(recovery_path);
            return Ok(json!({
                "status": "partial",
                "executed": true,
                "path": normalized,
                "recovery_path": recovery_path,
                "error": "File changed after preview and the patch could not be fully rolled back. Inspect the target and recovery paths before retrying."
            }));
        }
        let digest = hash(after.as_bytes());
        drop(temporary);
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("after_commit");
        }
        Ok(json!({"path":normalized,"sha256":digest,"bytes":after.len(),"executed":true}))
    }
    fn prepare(&self, name: &str, args: &Value) -> Result<(PreparedAction, ActionPlan), String> {
        schema::validate(name, args)?;
        if self.auto_denied {
            return Err("An action was denied. A new user request is required before proposing more actions".into());
        }
        if name == "apply_patch" {
            return self.prepare_patch(args);
        }
        let argv = schema::argv(&args["argv"])?;
        let policy = if self.approval_mode == ApprovalMode::FullAccess {
            ExecutionPolicy::FullAccess
        } else {
            if !self.allowed_commands.is_empty() && !self.allowed_commands.contains(&argv) {
                return Err("Command is outside the exact --allow-command policy".into());
            }
            ExecutionPolicy::Sandbox(self.sandbox_mode)
        };
        let mut prepared = PreparedCommand::new(&self.root, argv, policy)?;
        if args["http_get"] == true {
            prepared.enable_broker()?;
        }
        if let Some(fetch) = &prepared.fetch {
            self.check_network_origin(&fetch.origin())?;
        }
        let plan = prepared.plan();
        Ok((PreparedAction::Command(prepared), plan))
    }

    fn check_network_origin(&self, origin: &str) -> Result<(), String> {
        if self
            .denied_network_origins
            .iter()
            .any(|denied| denied == origin)
            || self.denied_network_in_session.contains(origin)
        {
            return Err(format!("Network destination {origin} is explicitly denied"));
        }
        Ok(())
    }

    fn check_command_root(&self) -> Result<(), String> {
        if !identity(
            &self.root.symlink_metadata().map_err(err)?,
            &self.root_fd.metadata().map_err(err)?,
        ) {
            return Err("Project root changed during session".into());
        }
        Ok(())
    }

    fn start_broker(&self, command: PreparedCommand) -> Result<BrokerJob, String> {
        self.check_command_root()?;
        let broker =
            Broker::new().map_err(|error| format!("Cannot start HTTP(S) GET broker: {error}"))?;
        let socket = broker.socket().to_owned();
        if socket.starts_with(&self.root) {
            return Err("HTTP(S) GET broker socket would be inside the project".into());
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or("Command timeout is too large")?;
        let cancel = self.cancel.child();
        let process_cancel = cancel.clone();
        let network_lifetime = cancel.child();
        let requests = network_lifetime.clone();
        let limit = self.output_limit;
        let worker = thread::Builder::new()
            .spawn(move || {
                let result = commands::run_command(
                    command,
                    &process_cancel,
                    deadline.saturating_duration_since(Instant::now()),
                    limit,
                    Some((&socket, &requests)),
                );
                requests.cancel();
                result
            })
            .map_err(err)?;
        Ok(BrokerJob {
            broker,
            worker: Some(worker),
            cancel,
            network_lifetime,
            deadline,
            requests: Vec::new(),
        })
    }

    fn prepare_broker_get(
        &self,
        fetch: &Fetch,
        parent: &ActionPlan,
    ) -> Result<(PreparedCommand, ActionPlan), String> {
        let scope = fetch.scope();
        self.check_network_origin(&fetch.origin())?;
        let prepared = PreparedCommand::new(
            &self.root,
            vec!["curl".into(), fetch.url().into()],
            ExecutionPolicy::Sandbox(self.sandbox_mode),
        )?;
        let mut plan = prepared.plan();
        plan.preview = format!(
            "Requested network destination = {scope}\nParent command\n{}",
            parent.preview
        );
        plan.evidence["parent_command"] = parent.evidence.clone();
        plan.evidence["effective_argv"] = json!(fetch.broker_args());
        plan.evidence["response_limit_bytes"] = json!(MAX_BROKER_BODY);
        Ok((prepared, plan))
    }

    fn serve_broker_get(
        job: &mut BrokerJob,
        request: Request,
        fetch: PreparedCommand,
        mut event: Value,
        approved: bool,
    ) {
        if !approved || !job.active() {
            event["status"] = json!(if job.active() { "denied" } else { "stopped" });
            request.respond(
                403,
                b"Network request was not approved for an active action",
            );
        } else {
            let outcome = commands::broker_get(
                fetch,
                &job.network_lifetime,
                job.deadline.saturating_duration_since(Instant::now()),
            );
            match outcome {
                Ok((code, body)) => {
                    event["status"] = json!("completed");
                    event["http_status"] = json!(code);
                    request.respond(code, &body);
                }
                Err(error) => {
                    event["status"] = json!("failed");
                    request.respond(502, error.as_bytes());
                }
            }
        }
        job.requests.push(event);
    }

    fn next_broker_get(
        &self,
        job: &mut BrokerJob,
        parent: &ActionPlan,
    ) -> Option<(Request, PreparedCommand, ActionPlan, String)> {
        let (request, fetch) = job.broker.next()?;
        if self.auto_denied {
            request.respond(403, b"An automatic review denied an action in this task");
            return None;
        }
        match self.prepare_broker_get(&fetch, parent) {
            Ok((prepared, plan)) => Some((request, prepared, plan, fetch.origin())),
            Err(error) => {
                job.requests
                    .push(json!({"status":"not_run","scope":fetch.scope(),"error":error}));
                request.respond(403, error.as_bytes());
                None
            }
        }
    }

    fn commit(&mut self, action: PreparedAction) -> Result<Value, String> {
        if self.cancel.is_canceled() {
            return Ok(unexecuted("canceled"));
        }
        match action {
            PreparedAction::Patch(patch) => self.commit_patch(patch),
            PreparedAction::Command(command) => {
                self.check_command_root()?;
                commands::run_prepared(command, &self.cancel, self.timeout, self.output_limit)
            }
        }
    }

    fn needs_approval(&self, action: &PreparedAction) -> bool {
        self.approval_mode != ApprovalMode::FullAccess
            && !matches!(action, PreparedAction::Command(command) if command.low_risk())
    }

    fn action_evidence(&self, name: &str, args: &Value, plan: &ActionPlan) -> Value {
        let definition = schema::tools()
            .as_array()
            .unwrap()
            .iter()
            .find(|def| def["function"]["name"] == name)
            .unwrap()["function"]["description"]
            .clone();
        json!({"tool":name,"description":definition,"validated_arguments":args,
            "project_root":self.root,"prepared":plan.evidence,
            "effective_policy":{"approval_mode":self.approval_mode,"command_sandbox":self.sandbox_mode,
                "exact_argv_restricted":!self.allowed_commands.is_empty(),"command_timeout_ms":self.timeout.as_millis(),
                "output_limit_bytes":self.output_limit,
                "additional_network_permission":plan.evidence["requested_network"]},
            "approval_required_because": if plan.path.is_some() { "This action changes a project file through the bounded patch executor" }
                else if !plan.evidence["requested_network"].is_null() { "This HTTP(S) GET transmits request data to a new destination for this action" }
                else { "This command is outside the fixed inspection exemption and can execute project code" }})
    }

    fn review_context(&self, reviewer: &Reviewer, action: Value, history: &[Value]) -> Value {
        let user: Vec<_> = history
            .iter()
            .filter(|message| message["role"] == "user")
            .map(|message| &message["content"])
            .collect();
        let mut evidence = json!({"prepared_action":action,"trusted_user_instructions":user,
            "recorded_human_decisions":self.human_decisions,
            "untrusted_recent_conversation":[]});
        let mut remaining = reviewer
            .evidence_budget()
            .saturating_sub(evidence.to_string().len());
        let mut recent = Vec::new();
        for message in history
            .iter()
            .rev()
            .filter(|m| matches!(m["role"].as_str(), Some("assistant" | "tool")))
            .take(8)
        {
            if remaining < 512 {
                break;
            }
            let content = message["content"].as_str().unwrap_or_default();
            let call_summary;
            let content = if content.is_empty() && message["tool_calls"].is_array() {
                call_summary = message["tool_calls"].to_string();
                &call_summary
            } else {
                content
            };
            let excerpt: String = content.chars().take(1200).collect();
            let entry = json!({"source":message["role"],"tool_name":message["tool_name"],
                "content_excerpt":excerpt,"truncated":excerpt.len() < content.len()});
            let size = entry.to_string().len() + 1;
            if size <= remaining {
                remaining -= size;
                recent.push(entry);
            }
        }
        recent.reverse();
        evidence["untrusted_recent_conversation"] = json!(recent);
        evidence
    }

    fn confirm(
        &mut self,
        plan: &ActionPlan,
        digest: &str,
        fallback: Option<&str>,
        origin: Option<&str>,
        cancel: &Cancellation,
    ) -> bool {
        if cancel.is_canceled() {
            return false;
        }
        let preview = match fallback {
            Some(reason) => format!(
                "{plan_preview}\nAutomatic review needs confirmation: {reason}",
                plan_preview = plan.preview
            ),
            None => plan.preview.clone(),
        };
        let allowed = (self.approve)(plan.kind, &preview);
        if cancel.is_canceled() {
            return false;
        }
        self.human_decisions.push(json!({"decision":if allowed { "allow" } else { "not_approved" },
            "action_sha256":digest,"scope_excerpt":plan.preview.chars().take(512).collect::<String>(),
            "network_origin":origin}));
        if !allowed {
            if let Some(origin) = origin {
                self.denied_network_in_session.insert(origin.to_owned());
            }
            self.auto_denied = self.approval_mode == ApprovalMode::Auto;
        }
        allowed
    }

    async fn authorize(
        &mut self,
        plan: &ActionPlan,
        evidence: Value,
        history: &[Value],
        origin: Option<&str>,
        cancel: &Cancellation,
    ) -> (bool, Value) {
        let digest = hash(evidence.to_string().as_bytes());
        let reviewer = self
            .reviewer
            .as_ref()
            .expect("automatic review was selected");
        let review = reviewer
            .review(&self.review_context(reviewer, evidence, history), cancel)
            .await;
        let (allowed, mut audit) = if cancel.is_canceled() {
            (false, json!({"decision":"canceled"}))
        } else {
            match review {
                Ok(verdict) => {
                    let allowed = match verdict.decision {
                        Decision::Allow => true,
                        Decision::Deny => {
                            if let Some(origin) = origin {
                                self.denied_network_in_session.insert(origin.to_owned());
                            }
                            self.auto_denied = true;
                            false
                        }
                        Decision::AskUser => {
                            self.confirm(plan, &digest, Some(&verdict.reason), origin, cancel)
                        }
                    };
                    (allowed, json!(verdict))
                }
                Err(error) => {
                    let reason = error.to_string();
                    (
                        self.confirm(plan, &digest, Some(&reason), origin, cancel),
                        json!({"decision":"ask_user","reason":reason}),
                    )
                }
            }
        };
        audit["action_sha256"] = json!(digest);
        (allowed, audit)
    }

    async fn serve_broker(
        &mut self,
        command: PreparedCommand,
        parent: &ActionPlan,
        args: &Value,
        history: &[Value],
    ) -> Result<Value, String> {
        let mut job = self.start_broker(command)?;
        while job.active() {
            if let Some((request, fetch, plan, origin)) = self.next_broker_get(&mut job, parent) {
                let evidence = self.action_evidence("run_command", args, &plan);
                let (allowed, audit) = self
                    .authorize(
                        &plan,
                        evidence,
                        history,
                        Some(&origin),
                        &job.network_lifetime,
                    )
                    .await;
                let event =
                    json!({"scope":plan.evidence["requested_network"],"automatic_review":audit});
                Self::serve_broker_get(&mut job, request, fetch, event, allowed);
            } else {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        job.finish()
    }

    fn serve_broker_sync(
        &mut self,
        command: PreparedCommand,
        parent: &ActionPlan,
        args: &Value,
    ) -> Result<Value, String> {
        let mut job = self.start_broker(command)?;
        while job.active() {
            if let Some((request, fetch, plan, origin)) = self.next_broker_get(&mut job, parent) {
                let evidence = self.action_evidence("run_command", args, &plan);
                let digest = hash(evidence.to_string().as_bytes());
                let fallback =
                    (self.approval_mode == ApprovalMode::Auto).then_some("Reviewer unavailable");
                let allowed = self.confirm(
                    &plan,
                    &digest,
                    fallback,
                    Some(&origin),
                    &job.network_lifetime,
                );
                let event = json!({"scope":plan.evidence["requested_network"]});
                Self::serve_broker_get(&mut job, request, fetch, event, allowed);
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        job.finish()
    }

    pub(crate) fn begin_task(&mut self) {
        self.auto_denied = false;
    }

    pub(crate) async fn execute_with_history(
        &mut self,
        name: &str,
        args: Value,
        history: &[Value],
    ) -> Value {
        if self.approval_mode != ApprovalMode::Auto
            || self.reviewer.is_none()
            || self.cancel.is_canceled()
        {
            return self.execute(name, args);
        }
        let (action, plan) = match self.prepare(name, &args) {
            Ok(prepared) => prepared,
            Err(error) => return self.record_result(name, args, Err(error)),
        };
        if !self.needs_approval(&action) {
            let result = self.commit(action);
            return self.record_result(name, args, result);
        }
        let cancel = self.cancel.clone();
        let origin = action.origin();
        let evidence = self.action_evidence(name, &args, &plan);
        let (allowed, audit) = self
            .authorize(&plan, evidence, history, origin.as_deref(), &cancel)
            .await;
        let result = if cancel.is_canceled() {
            Ok(plan.stopped("canceled"))
        } else if !allowed {
            Ok(plan.stopped("denied"))
        } else {
            match action {
                PreparedAction::Command(command) if command.broker => {
                    self.serve_broker(command, &plan, &args, history).await
                }
                _ => self.commit(action),
            }
        };
        let mut result = finish_result(name, result, &self.cancel);
        result["automatic_review"] = audit;
        self.record_result(name, args, Ok(result))
    }
    pub(crate) fn record_result(
        &mut self,
        name: &str,
        args: Value,
        result: Result<Value, String>,
    ) -> Value {
        let result = finish_result(name, result, &self.cancel);
        if result["status"] == "cleanup_failed" {
            self.cancel.cancel();
        }
        self.events
            .push(json!({"tool":name,"arguments":args,"result":result}));
        result
    }
    /// Synchronous entry point for manual callers. Automatic inference runs through Agent.
    pub fn execute(&mut self, name: &str, args: Value) -> Value {
        let result = if self.cancel.is_canceled() {
            Ok(unexecuted("canceled"))
        } else {
            self.prepare(name, &args).and_then(|(action, plan)| {
                if !self.needs_approval(&action) {
                    return self.commit(action);
                }
                let digest = hash(
                    self.action_evidence(name, &args, &plan)
                        .to_string()
                        .as_bytes(),
                );
                let fallback =
                    (self.approval_mode == ApprovalMode::Auto).then_some("Reviewer unavailable");
                let cancel = self.cancel.clone();
                let origin = action.origin();
                let allowed = self.confirm(&plan, &digest, fallback, origin.as_deref(), &cancel);
                if cancel.is_canceled() {
                    return Ok(plan.stopped("canceled"));
                }
                if !allowed {
                    return Ok(plan.stopped("denied"));
                }
                match action {
                    PreparedAction::Command(command) if command.broker => {
                        self.serve_broker_sync(command, &plan, &args)
                    }
                    _ => self.commit(action),
                }
            })
        };
        self.record_result(name, args, result)
    }
}
