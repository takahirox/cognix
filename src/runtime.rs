use crate::{Cancellation, commands, schema};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::{CStr, CString},
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

pub type Approval = Box<dyn FnMut(&str, &str) -> bool>;
#[cfg(test)]
type CommitHook = Box<dyn FnMut(&str)>;
pub struct Runtime {
    pub root: PathBuf,
    root_fd: File,
    pub approve: Approval,
    pub cancel: Cancellation,
    pub allowed_commands: Vec<Vec<String>>,
    pub timeout: Duration,
    pub output_limit: usize,
    pub seen: HashMap<String, String>,
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
struct Temporary<'a> {
    parent: &'a File,
    name: CString,
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

// Creation owns a descriptor as well as a name. Cleanup must not unlink a file
// another process substituted for our staging file.
struct CreationTemporary<'a> {
    parent: &'a File,
    name: CString,
    file: File,
}
impl CreationTemporary<'_> {
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
impl Drop for CreationTemporary<'_> {
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
            modified: BTreeSet::new(),
            events: vec![],
            #[cfg(test)]
            hook: None,
        })
    }
    fn parts(path: &str, directory: bool) -> Result<Vec<&str>, String> {
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
        if !directory && parts.is_empty() {
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
        if info.len() > 65536 {
            return Err("File exceeds 64 KiB".into());
        }
        let mut raw = Vec::new();
        file.take(65537).read_to_end(&mut raw).map_err(err)?;
        if raw.len() > 65536 {
            return Err("File exceeds 64 KiB".into());
        }
        Ok((raw, info))
    }
    fn list(&self, path: &str) -> Result<Value, String> {
        let directory = self.directory(&Self::parts(path, true)?)?;
        // fdopendir takes ownership. Duplicate rather than sharing a directory offset
        // with the anchored root descriptor (dup alone shares offsets).
        let listing = openat(&directory, ".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        use std::os::fd::IntoRawFd;
        let raw = listing.into_raw_fd();
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            unsafe {
                libc::close(raw);
            }
            return Err(err(std::io::Error::last_os_error()));
        }
        struct Dir(*mut libc::DIR);
        impl Drop for Dir {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let guard = Dir(dir);
        let mut entries = Vec::new();
        let mut bytes = 0;
        loop {
            if self.cancel.is_canceled() {
                return Ok(json!({"status":"canceled","executed":false}));
            }
            let entry = unsafe { libc::readdir(guard.0) };
            if entry.is_null() {
                break;
            }
            let raw_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = raw_name
                .to_str()
                .map_err(|_| "Non-UTF8 file name is unsupported")?;
            if matches!(name, "." | "..") || name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    raw_name.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                return Err(err(std::io::Error::last_os_error()));
            }
            let st = unsafe { st.assume_init() };
            if st.st_mode & libc::S_IFMT == libc::S_IFLNK {
                continue;
            }
            let display = format!(
                "{name}{}",
                if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
                    "/"
                } else {
                    ""
                }
            );
            bytes += display.len();
            entries.push(display);
            if entries.len() > 500 || bytes > self.output_limit {
                return Err("Directory listing exceeds limit; choose a narrower directory".into());
            }
        }
        entries.sort();
        Ok(json!({"entries":entries}))
    }
    fn read_file(&mut self, path: &str) -> Result<Value, String> {
        let parts = Self::parts(path, false)?;
        let parent = self.directory(&parts[..parts.len() - 1])?;
        let (raw, _) = Self::read(&parent, parts[parts.len() - 1])?;
        let text = std::str::from_utf8(&raw).map_err(err)?;
        if text.contains('\0') {
            return Err("Binary files are unsupported".into());
        }
        let digest = hash(&raw);
        self.seen.insert(parts.join("/"), digest.clone());
        Ok(json!({"content":text,"sha256":digest}))
    }
    fn patch(&mut self, a: &Value) -> Result<Value, String> {
        let parts = Self::parts(a["path"].as_str().unwrap(), false)?;
        let normalized = parts.join("/");
        let digest = a["sha256"].as_str().unwrap();
        if self.seen.get(&normalized).map(String::as_str) != Some(digest) {
            return Err("Read the file first and use its current sha256".into());
        }
        let old = a["old_text"].as_str().unwrap();
        let new = a["new_text"].as_str().unwrap();
        if old.is_empty() || old == new || old.contains('\0') || new.contains('\0') {
            return Err("Patch needs a nonempty changed span without NULs".into());
        }
        let parent = self.directory(&parts[..parts.len() - 1])?;
        let name = parts[parts.len() - 1];
        let (raw, info) = Self::read(&parent, name)?;
        if hash(&raw) != digest {
            return Err("File changed since read; read again".into());
        }
        let before = std::str::from_utf8(&raw).map_err(err)?;
        if before.matches(old).count() != 1 {
            return Err("old_text must match exactly once".into());
        }
        let after = before.replacen(old, new, 1);
        if after.len() > 65536 {
            return Err("Patched file exceeds 64 KiB".into());
        }
        let diff = similar::TextDiff::from_lines(before, &after)
            .unified_diff()
            .header(&normalized, &normalized)
            .to_string();
        let preview = format!("{diff}\nExact replacement: {old:?} -> {new:?}");
        let approved = (self.approve)("patch", &preview);
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","executed":false}));
        }
        if !approved {
            return Ok(json!({"status":"denied","path":normalized,"executed":false}));
        }
        let fresh = self.directory(&parts[..parts.len() - 1])?;
        if !identity(
            &fresh.metadata().map_err(err)?,
            &parent.metadata().map_err(err)?,
        ) {
            return Err("Parent directory changed during approval".into());
        }
        let (current, current_info) = Self::read(&parent, name)?;
        if current != raw || !identity(&current_info, &info) {
            return Err("File changed during approval; patch not applied".into());
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp_name = format!(
            ".cognix-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut file = openat(
            &parent,
            &temp_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            info.mode() & 0o777,
        )?;
        let temporary = Temporary {
            parent: &parent,
            name: cstring(&temp_name)?,
        };
        file.set_permissions(std::fs::Permissions::from_mode(info.mode() & 0o777))
            .map_err(err)?;
        file.write_all(after.as_bytes()).map_err(err)?;
        file.sync_all().map_err(err)?;
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("before_commit");
        }
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","executed":false}));
        }
        let target = cstring(name)?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.name.as_ptr(),
                parent.as_raw_fd(),
                target.as_ptr(),
            )
        } < 0
        {
            return Err(err(std::io::Error::last_os_error()));
        }
        // No fallible/canceling operation between commit and ownership of evidence.
        self.modified.insert(normalized.clone());
        let digest = hash(after.as_bytes());
        self.seen.insert(normalized.clone(), digest.clone());
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("after_commit");
        }
        Ok(json!({"path":normalized,"sha256":digest,"executed":true}))
    }
    fn creation_parent_unchanged(&self, parts: &[&str], parent: &File) -> Result<(), String> {
        let root = self.root.symlink_metadata().map_err(err)?;
        if !root.is_dir() || !identity(&root, &self.root_fd.metadata().map_err(err)?) {
            return Err("Project root changed during session".into());
        }
        let fresh = self.directory(parts)?;
        if !identity(
            &fresh.metadata().map_err(err)?,
            &parent.metadata().map_err(err)?,
        ) {
            return Err("Parent directory changed during creation".into());
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
    fn create_file(&mut self, args: &Value) -> Result<Value, String> {
        let path = args["path"].as_str().unwrap();
        let parts = Self::parts(path, false)?;
        if matches!(path.rsplit('/').next(), Some("" | ".")) {
            return Err("Expected a destination file name".into());
        }
        let normalized = parts.join("/");
        let content = args["content"].as_str().unwrap();
        if content.contains('\0') {
            return Err("New file content must be UTF-8 text without NULs".into());
        }
        let parent_parts = &parts[..parts.len() - 1];
        let parent = self.directory(parent_parts)?;
        let target = cstring(parts[parts.len() - 1])?;
        self.creation_parent_unchanged(parent_parts, &parent)?;
        Self::destination_absent(&parent, &target)?;

        // Bound the escaped display, not the raw text: terminal controls can
        // expand when sanitized. The complete content stays in the event ledger.
        const PREVIEW_CHARS: usize = 8192;
        let display = crate::terminal::text(content);
        let excerpt: String = display.chars().take(PREVIEW_CHARS).collect();
        let mut preview = format!(
            "Create {normalized:?} ({} UTF-8 bytes)\n--- /dev/null\n+++ {normalized:?}\n",
            content.len()
        );
        for line in excerpt.split_inclusive('\n') {
            preview.push('+');
            preview.push_str(line);
        }
        if !excerpt.ends_with('\n') && !excerpt.is_empty() {
            preview.push_str("\n\\ No newline at end of displayed content\n");
        }
        if display.chars().count() > PREVIEW_CHARS {
            preview.push_str("[Creation preview truncated; full content is larger]\n");
        } else if content.is_empty() {
            preview.push_str("[Empty file: 0 additions]\n");
        }
        let approved = (self.approve)("create_file", &preview);
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","path":normalized,"executed":false}));
        }
        if !approved {
            return Ok(json!({"status":"denied","path":normalized,"executed":false}));
        }
        self.creation_parent_unchanged(parent_parts, &parent)?;
        Self::destination_absent(&parent, &target)?;

        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            ".cognix-create-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let file = openat(
            &parent,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let mut temporary = CreationTemporary {
            parent: &parent,
            name: cstring(&name)?,
            file,
        };
        temporary.file.write_all(content.as_bytes()).map_err(err)?;
        temporary.file.sync_all().map_err(err)?;
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("before_commit");
        }
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","path":normalized,"executed":false}));
        }
        self.creation_parent_unchanged(parent_parts, &parent)?;
        if !temporary.owns_name() || temporary.file.metadata().map_err(err)?.nlink() != 1 {
            return Err("Creation staging file changed before commit".into());
        }
        // linkat publishes the fully written inode atomically and fails with
        // EEXIST for *every* existing destination, including dangling symlinks.
        // The earlier absence checks are only for feedback, never exclusivity.
        if unsafe {
            libc::linkat(
                parent.as_raw_fd(),
                temporary.name.as_ptr(),
                parent.as_raw_fd(),
                target.as_ptr(),
                0,
            )
        } < 0
        {
            return Err(err(std::io::Error::last_os_error()));
        }
        // No fallible/canceling operation between publish and evidence ownership.
        let digest = hash(content.as_bytes());
        self.modified.insert(normalized.clone());
        // Keep apply_patch's read-first contract, including stale observations
        // of a previous file that an external editor removed at this path.
        self.seen.remove(&normalized);
        drop(temporary);
        #[cfg(test)]
        if let Some(h) = &mut self.hook {
            h("after_commit");
        }
        Ok(json!({"path":normalized,"sha256":digest,"bytes":content.len(),"executed":true}))
    }
    fn run_command(&mut self, args: &Value) -> Result<Value, String> {
        let argv = schema::argv(&args["argv"])?;
        if !self.allowed_commands.contains(&argv) {
            return Ok(
                json!({"status":"not_run","executed":false,"error":"Command is outside the exact --allow-command policy"}),
            );
        }
        commands::sandbox_argv(&self.root, &self.root, &argv)?;
        let approved = (self.approve)(
            "command",
            &format!(
                "argv = {}\nRead-only project; no network; scratch writes only.",
                json!(argv)
            ),
        );
        if self.cancel.is_canceled() {
            return Ok(json!({"status":"canceled","executed":false}));
        }
        if !approved {
            return Ok(json!({"status":"denied","executed":false}));
        }
        if !identity(
            &self.root.metadata().map_err(err)?,
            &self.root_fd.metadata().map_err(err)?,
        ) {
            return Err("Project root changed during session".into());
        }
        commands::run(
            &self.root,
            &argv,
            &self.cancel,
            self.timeout,
            self.output_limit,
        )
    }
    pub fn execute(&mut self, name: &str, args: Value) -> Value {
        let result = if self.cancel.is_canceled() {
            Ok(json!({"status":"canceled","executed":false}))
        } else {
            schema::validate(name, &args).and_then(|()| match name {
                "list_files" => self.list(args["path"].as_str().unwrap()),
                "read_file" => self.read_file(args["path"].as_str().unwrap()),
                "apply_patch" => self.patch(&args),
                "create_file" => self.create_file(&args),
                "run_command" => self.run_command(&args),
                _ => Err("Unknown tool".into()),
            })
        };
        let mut result=result.unwrap_or_else(|e|json!({"status":if name=="run_command"{"not_run"}else{"failed"},"executed":false,"error":e.chars().take(1000).collect::<String>()}));
        if result.get("status").is_none() {
            result["status"] = json!("ok");
        }
        if self.cancel.is_canceled() && matches!(result["status"].as_str(), Some("ok" | "passed")) {
            result["status"] = json!("canceled");
        }
        self.events
            .push(json!({"tool":name,"arguments":args,"result":result}));
        result
    }
}
