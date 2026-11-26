use crate::{
    Cancellation,
    action::{ActionPlan, finish_result, unexecuted},
    commands, schema,
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
    time::Duration,
};
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;

// Internal milestone-2 preparation only; deliberately absent from tool dispatch.
#[allow(dead_code)]
#[path = "patch.rs"]
pub(crate) mod patch;

pub use crate::action::Approval;
#[cfg(test)]
type CommitHook = Box<dyn FnMut(&str)>;
pub struct Runtime {
    pub root: PathBuf,
    root_fd: File,
    pub approve: Approval,
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
// Own both the inode and its staging name so cleanup can check identity.
struct PatchTemporary<'a> {
    parent: &'a File,
    name: CString,
    file: File,
}
impl PatchTemporary<'_> {
    fn owns_name(&self) -> bool {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        let Ok(info) = self.file.metadata() else {
            return false;
        };
        // SAFETY: live descriptors/name and valid stat output storage.
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
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
}
impl Drop for PatchTemporary<'_> {
    fn drop(&mut self) {
        if self.owns_name() {
            unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }
}

impl Runtime {
    pub fn new(root: &Path, approve: Approval, cancel: Cancellation) -> Result<Self, String> {
        let root = root.canonicalize().map_err(err)?;
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
    fn read(parent: &File, name: &str) -> Result<(Vec<u8>, Metadata), String> {
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
        Ok((raw, info))
    }
    #[cfg(test)]
    fn observe_patch_source(&mut self, path: &str) -> Result<String, String> {
        let parts = Self::parts(path)?;
        let (name, parent_parts) = parts.split_last().unwrap();
        let parent = self.directory(parent_parts)?;
        let (raw, info) = Self::read(&parent, name)?;
        let text = std::str::from_utf8(&raw).map_err(err)?;
        if text.contains('\0') {
            return Err("Binary files are unsupported".into());
        }
        let digest = hash(&raw);
        self.patch_reads.insert(
            parts.join("/"),
            patch::ReadEvidence::new(&parent, name, &info, &digest),
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
        before: Option<&(Vec<u8>, Metadata)>,
    ) -> Result<(), String> {
        let Some((raw, info)) = before else {
            return Self::destination_absent(parent, target);
        };
        let changed = "File changed after preview. Prepare a new patch against the current file";
        let (current, current_info) = Self::read(parent, name).map_err(|_| changed)?;
        if current != *raw
            || patch::Fingerprint::new(&current_info) != patch::Fingerprint::new(info)
        {
            return Err(changed.into());
        }
        Ok(())
    }
    fn patch(&mut self, args: &Value) -> Result<Value, String> {
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
            let text = std::str::from_utf8(&snapshot.0).map_err(err)?;
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
            let plan = ActionPlan::patch(&normalized, text, &after, old, new);
            (Some(snapshot), after, plan)
        } else {
            Self::destination_absent(&parent, &target)?;
            (None, new.to_owned(), ActionPlan::creation(&normalized, new))
        };
        if let Err(result) = plan.approve(&mut self.approve, &self.cancel) {
            return Ok(result);
        }
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
        };
        temporary.file.write_all(after.as_bytes()).map_err(err)?;
        if let Some((_, info)) = &before {
            temporary
                .file
                .set_permissions(std::fs::Permissions::from_mode(info.mode() & 0o777))
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

        let fd = parent.as_raw_fd();
        // Both operations publish complete contents at once. linkat also
        // enforces destination absence at the syscall, including dangling links.
        let result = unsafe {
            if before.is_some() {
                libc::renameat(fd, temporary.name.as_ptr(), fd, target.as_ptr())
            } else {
                libc::linkat(fd, temporary.name.as_ptr(), fd, target.as_ptr(), 0)
            }
        };
        if result < 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        // Own the evidence before cleanup or reporting cancellation.
        self.modified.insert(normalized.clone());
        let digest = hash(after.as_bytes());
        self.seen.remove(&normalized);
        self.patch_reads.remove(&normalized);
        drop(temporary);
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("after_commit");
        }
        Ok(json!({"path":normalized,"sha256":digest,"bytes":after.len(),"executed":true}))
    }
    fn run_command(&mut self, args: &Value) -> Result<Value, String> {
        let argv = schema::argv(&args["argv"])?;
        if !self.allowed_commands.is_empty() && !self.allowed_commands.contains(&argv) {
            return Ok(
                json!({"status":"not_run","executed":false,"error":"Command is outside the exact --allow-command policy"}),
            );
        }
        let prepared = commands::PreparedCommand::new(&argv)?;
        if let Err(result) =
            ActionPlan::command(&argv, prepared.invocation(), prepared.executable())
                .approve(&mut self.approve, &self.cancel)
        {
            return Ok(result);
        }
        if !identity(
            &self.root.symlink_metadata().map_err(err)?,
            &self.root_fd.metadata().map_err(err)?,
        ) {
            return Err("Project root changed during session".into());
        }
        commands::run_prepared(
            &self.root,
            prepared,
            &self.cancel,
            self.timeout,
            self.output_limit,
        )
    }
    pub(crate) fn record_result(
        &mut self,
        name: &str,
        args: Value,
        result: Result<Value, String>,
    ) -> Value {
        let result = finish_result(name, result, &self.cancel);
        self.events
            .push(json!({"tool":name,"arguments":args,"result":result}));
        result
    }
    pub fn execute(&mut self, name: &str, args: Value) -> Value {
        let result = if self.cancel.is_canceled() {
            Ok(unexecuted("canceled"))
        } else {
            schema::validate(name, &args).and_then(|()| match name {
                "apply_patch" => self.patch(&args),
                "run_command" => self.run_command(&args),
                _ => Err("Unknown tool".into()),
            })
        };
        self.record_result(name, args, result)
    }
}
