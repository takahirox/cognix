//! Internal v2 preparation. No approval, staging, or mutation capability lives here.
use super::{Runtime, hash, openat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io::Read,
    os::{
        fd::{AsRawFd, IntoRawFd},
        unix::fs::MetadataExt,
    },
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

pub(crate) const FILE_BYTES: usize = 65536;
const ARG_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 256 * 1024;
const PREVIEW_CHARS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    Json,
    Shape,
    Version,
    Limit,
    Path,
    Hash,
    Text,
    Edit,
    Conflict,
    ReadRequired,
    StaleState,
    UnsupportedMetadata,
    Filesystem,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PatchError {
    pub reason: Reason,
    pub location: String,
    pub detail: String,
}
type Result<T> = std::result::Result<T, PatchError>;
fn error(reason: Reason, location: &str, detail: impl std::fmt::Display) -> PatchError {
    PatchError {
        reason,
        location: location.into(),
        detail: crate::terminal::text(&detail.to_string()),
    }
}
fn io(location: &str, e: impl std::fmt::Display) -> PatchError {
    error(Reason::Filesystem, location, e)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u64,
    changes: Vec<Change>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum Change {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        sha256: String,
        edits: Vec<Edit>,
    },
    Delete {
        path: String,
        sha256: String,
    },
    Move {
        path: String,
        to: String,
        sha256: String,
    },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    old_text: String,
    new_text: String,
}
impl Change {
    fn path(&self) -> &str {
        match self {
            Self::Add { path, .. }
            | Self::Update { path, .. }
            | Self::Delete { path, .. }
            | Self::Move { path, .. } => path,
        }
    }
    fn destination(&self) -> Option<&str> {
        match self {
            Self::Add { path, .. } => Some(path),
            Self::Move { to, .. } => Some(to),
            _ => None,
        }
    }
    fn digest(&self) -> Option<&str> {
        match self {
            Self::Add { .. } => None,
            Self::Update { sha256, .. }
            | Self::Delete { sha256, .. }
            | Self::Move { sha256, .. } => Some(sha256),
        }
    }
    fn edits(&self) -> &[Edit] {
        match self {
            Self::Update { edits, .. } => edits,
            _ => &[],
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Update { .. } => "update",
            Self::Delete { .. } => "delete",
            Self::Move { .. } => "move",
        }
    }
}
/// Constructible only through the pure, bounded parser.
#[derive(Debug)]
pub(crate) struct PatchIntent {
    changes: Vec<Change>,
}
fn folded(path: &str) -> String {
    path.nfd()
        .flat_map(char::to_lowercase)
        .case_fold()
        .nfd()
        .collect()
}
fn validate_path(path: &str) -> Result<()> {
    if path.len() > 4096 || path.split('/').count() > 32 {
        return Err(error(
            Reason::Limit,
            path,
            "Path exceeds 4096 bytes or 32 components",
        ));
    }
    if path.contains('\0')
        || path
            .split('/')
            .any(|c| c.is_empty() || matches!(c, "." | "..") || c.eq_ignore_ascii_case(".git"))
    {
        return Err(error(
            Reason::Path,
            path,
            "Use nonempty relative components without . .. or .git",
        ));
    }
    Ok(())
}
fn validate_text(text: &str, location: &str) -> Result<()> {
    if text.len() > FILE_BYTES {
        return Err(error(Reason::Limit, location, "Text exceeds 64 KiB"));
    }
    if text.contains('\0') {
        return Err(error(Reason::Text, location, "NUL text is unsupported"));
    }
    Ok(())
}
impl PatchIntent {
    pub(crate) fn parse(arguments: &[u8]) -> Result<Self> {
        if arguments.len() > ARG_BYTES {
            return Err(error(Reason::Limit, "$", "Arguments exceed 1 MiB"));
        }
        std::str::from_utf8(arguments).map_err(|e| error(Reason::Text, "$", e))?;
        let raw: Envelope = serde_json::from_slice(arguments).map_err(|e| {
            error(
                if e.is_data() {
                    Reason::Shape
                } else {
                    Reason::Json
                },
                "$",
                e,
            )
        })?;
        if raw.version != 2 {
            return Err(error(
                Reason::Version,
                "version",
                "Only version 2 is supported",
            ));
        }
        if raw.changes.is_empty() || raw.changes.len() > 8 {
            return Err(error(Reason::Limit, "changes", "Expected 1–8 changes"));
        }
        let mut paths = Vec::new();
        let mut total = 0;
        for (i, change) in raw.changes.iter().enumerate() {
            let location = format!("changes[{i}]");
            validate_path(change.path())?;
            paths.push(change.path());
            if let Change::Move { to, .. } = change {
                validate_path(to)?;
                paths.push(to);
            }
            if change.digest().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) {
                return Err(error(
                    Reason::Hash,
                    &location,
                    "sha256 must be 64 lowercase hex characters",
                ));
            }
            if let Change::Add { content, .. } = change {
                validate_text(content, &location)?;
                total += content.len();
            }
            let edits = change.edits();
            if edits.len() > 16 || (matches!(change, Change::Update { .. }) && edits.is_empty()) {
                return Err(error(
                    Reason::Limit,
                    &location,
                    "Expected 1–16 update edits",
                ));
            }
            for (j, edit) in edits.iter().enumerate() {
                let location = format!("{location}.edits[{j}]");
                validate_text(&edit.old_text, &location)?;
                validate_text(&edit.new_text, &location)?;
                if edit.old_text.is_empty() || edit.old_text == edit.new_text {
                    return Err(error(
                        Reason::Edit,
                        &location,
                        "Edit needs a nonempty anchor and different replacement",
                    ));
                }
            }
        }
        if total > TOTAL_BYTES {
            return Err(error(
                Reason::Limit,
                "changes",
                "Total resulting content exceeds 256 KiB",
            ));
        }
        // Conservative case/canonical-equivalence policy on every volume. Actual
        // descriptor identities below additionally catch filesystem-specific aliases.
        for (i, path) in paths.iter().enumerate() {
            let a = folded(path);
            for other in &paths[..i] {
                let b = folded(other);
                if a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a.clone() + "/"))
                {
                    return Err(error(
                        Reason::Conflict,
                        path,
                        format!("Path conflicts with {other:?}"),
                    ));
                }
            }
        }
        Ok(Self {
            changes: raw.changes,
        })
    }
}
fn edited(before: &str, edits: &[Edit], location: &str) -> Result<String> {
    let mut ranges = Vec::new();
    for edit in edits {
        let Some(start) = before.find(&edit.old_text) else {
            return Err(error(
                Reason::Edit,
                location,
                "Anchor is absent from original bytes",
            ));
        };
        // Search again one Unicode character after the first start, so overlapping
        // occurrences (aa in aaa) count without quadratic byte-window comparisons.
        let next = start + before[start..].chars().next().unwrap().len_utf8();
        if before[next..].contains(&edit.old_text) {
            return Err(error(
                Reason::Edit,
                location,
                "Anchor must occur exactly once in original bytes",
            ));
        }
        ranges.push((start, start + edit.old_text.len(), edit.new_text.as_str()));
    }
    ranges.sort_by_key(|r| r.0);
    if ranges.windows(2).any(|r| r[0].1 > r[1].0) {
        return Err(error(
            Reason::Edit,
            location,
            "Original-byte edit ranges overlap",
        ));
    }
    let size = ranges.iter().fold(before.len() as i64, |n, (a, b, s)| {
        n - (b - a) as i64 + s.len() as i64
    });
    if size > FILE_BYTES as i64 {
        return Err(error(Reason::Limit, location, "Result exceeds 64 KiB"));
    }
    let mut out = String::with_capacity(size as usize);
    let mut end = 0;
    for (a, b, replacement) in ranges {
        out.push_str(&before[end..a]);
        out.push_str(replacement);
        end = b;
    }
    out.push_str(&before[end..]);
    if !edits.is_empty() && out == before {
        return Err(error(Reason::Edit, location, "Combined edits are a no-op"));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn new(m: &Metadata) -> Self {
        Self {
            device: m.dev(),
            inode: m.ino(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Fingerprint {
    identity: Identity,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Fingerprint {
    fn new(m: &Metadata) -> Self {
        Self {
            identity: Identity::new(m),
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
            links: m.nlink(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }
    }
}
#[derive(Debug, Clone)]
pub(super) struct ReadEvidence {
    state: Fingerprint,
    sha256: String,
    metadata: Option<MetadataObservation>,
}
impl ReadEvidence {
    pub(super) fn new(parent: &File, name: &str, m: &Metadata, sha256: &str) -> Self {
        // Additional v2 evidence must never change legacy read_file acceptance.
        // A raced or failed descriptor capture leaves v2 evidence unavailable.
        let state = Fingerprint::new(m);
        let metadata = (|| {
            let file = openat(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0).ok()?;
            if Fingerprint::new(&file.metadata().ok()?) != state {
                return None;
            }
            let observed = inspect_metadata(&file, m);
            (Fingerprint::new(&file.metadata().ok()?) == state).then_some(observed)
        })();
        Self {
            state,
            sha256: sha256.into(),
            metadata,
        }
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Source {
    state: Fingerprint,
    metadata: MetadataObservation,
    content: String,
    sha256: String,
}
#[derive(Debug, Serialize)]
struct Operation {
    op: String,
    path: String,
    to: Option<String>,
    before: Option<Source>,
    after: Option<String>,
    after_sha256: Option<String>,
    before_bytes: usize,
    after_bytes: usize,
    destination_absent: bool,
    mode: Option<u32>,
    edits: Vec<Value>,
    constraints: Vec<&'static str>,
}
#[derive(Debug, Serialize)]
struct NewDirectory {
    path: String,
    first_change: usize,
    mode: u32,
}
#[derive(Debug, Serialize)]
pub(crate) struct PatchPlan {
    version: u8,
    root: Fingerprint,
    parents: BTreeMap<String, Fingerprint>,
    new_directories: Vec<NewDirectory>,
    operations: Vec<Operation>,
    total_result_bytes: usize,
    constraints: Vec<&'static str>,
}
impl PatchPlan {
    /// Complete owned inspection value; changing it cannot change this plan.
    pub(crate) fn inspect(&self) -> Value {
        serde_json::to_value(self).expect("serializable plan")
    }
    pub(crate) fn digest(&self) -> String {
        hash(self.inspect().to_string().as_bytes())
    }
    /// All untrusted characters escaped, including bidi/format characters.
    pub(crate) fn complete_escaped(&self) -> String {
        escape(&self.inspect().to_string())
    }
    pub(crate) fn preview(&self) -> String {
        let mut out = format!("v2 patch plan {} (not authorization)\n", self.digest());
        for dir in &self.new_directories {
            out.push_str(&format!(
                "mkdir {} (0700 subject to umask; change {})\n",
                quoted(&dir.path),
                dir.first_change
            ));
        }
        for (i, op) in self.operations.iter().enumerate() {
            out.push_str(&format!(
                "{i}: {} {}{} ({} -> {} bytes)\n",
                op.op,
                quoted(&op.path),
                op.to
                    .as_ref()
                    .map(|p| format!(" -> {}", quoted(p)))
                    .unwrap_or_default(),
                op.before_bytes,
                op.after_bytes
            ));
            if op.op == "move" {
                out.push_str(&format!(
                    "  unchanged content: sha256 {} ({} bytes)\n",
                    op.after_sha256.as_ref().unwrap(),
                    op.after_bytes
                ));
            }
            if let Some(source) = &op.before {
                out.push_str(&format!("  metadata: {}\n", source.metadata.summary()));
            }
            for constraint in &op.constraints {
                out.push_str(&format!("  constraint: {constraint}\n"));
            }
        }
        out.push_str("Content diffs (8192 escaped-character budget):\n");
        let mut budget = PREVIEW_CHARS;
        let mut truncated = false;
        for op in &self.operations {
            if op.op == "move" {
                continue;
            }
            let before = op.before.as_ref().map(|s| s.content.as_str()).unwrap_or("");
            let after = op.after.as_deref().unwrap_or("");
            let from = if op.before.is_none() {
                "/dev/null"
            } else {
                &op.path
            };
            let to = if op.after.is_none() {
                "/dev/null"
            } else {
                op.to.as_deref().unwrap_or(&op.path)
            };
            // A newline in a path must not manufacture extra diff headers.
            let diff = crate::action::patch_diff(&escape(from), &escape(to), before, after);
            let display: String = diff
                .chars()
                .map(|c| {
                    if c == '\n' {
                        "\n".into()
                    } else {
                        c.escape_debug().to_string()
                    }
                })
                .collect();
            let count = display.chars().count();
            out.extend(display.chars().take(budget));
            truncated |= count > budget;
            budget = budget.saturating_sub(count);
        }
        if truncated {
            out.push_str(
                "\n[Content preview truncated; inspect complete_escaped() before approval]\n",
            );
        }
        out
    }
}
fn escape(text: &str) -> String {
    text.chars().flat_map(char::escape_debug).collect()
}
fn quoted(text: &str) -> String {
    format!("\"{}\"", escape(text))
}

struct Inspection<'a> {
    runtime: &'a Runtime,
    root: Fingerprint,
    parents: BTreeMap<String, Fingerprint>,
    missing: Vec<NewDirectory>,
    // Parent identity (or planned parent path), folded child -> exact spelling.
    names: BTreeMap<(String, String), String>,
    sources: HashSet<(u64, u64)>,
    targets: HashSet<(String, String)>,
}
impl<'a> Inspection<'a> {
    fn root_unchanged(&self) -> Result<()> {
        let disk = self
            .runtime
            .root
            .symlink_metadata()
            .map_err(|e| io("root", e))?;
        let fd = self.runtime.root_fd.metadata().map_err(|e| io("root", e))?;
        if !disk.is_dir()
            || Fingerprint::new(&disk) != self.root
            || Fingerprint::new(&fd) != self.root
        {
            return Err(error(
                Reason::StaleState,
                "root",
                "Anchored project root changed",
            ));
        }
        Ok(())
    }
    fn register(&mut self, parent: &str, name: &str, location: &str) -> Result<()> {
        let key = (parent.into(), folded(name));
        match self.names.insert(key, name.into()) {
            Some(previous) if previous != name => {
                return Err(error(
                    Reason::Conflict,
                    location,
                    format!("Aliased component {previous:?}"),
                ));
            }
            _ => {}
        }
        Ok(())
    }
    fn parent(&mut self, path: &str, create: bool, change: usize) -> Result<(File, bool)> {
        let components: Vec<_> = path.split('/').collect();
        let mut fd = self.runtime.root_fd.try_clone().map_err(|e| io(path, e))?;
        let mut prefix = String::new();
        let mut missing = false;
        for component in &components[..components.len() - 1] {
            let parent_key = if missing {
                format!("planned:{prefix}")
            } else {
                let m = fd.metadata().map_err(|e| io(path, e))?;
                format!("{}:{}", m.dev(), m.ino())
            };
            name_feasible(&fd, component, path)?;
            self.register(&parent_key, component, path)?;
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if !missing {
                let entry = lookup(&fd, component, path)?;
                if entry.is_some() {
                    fd = openat(&fd, component, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                        .map_err(|e| io(path, e))?;
                    let state = Fingerprint::new(&fd.metadata().map_err(|e| io(path, e))?);
                    if self
                        .parents
                        .insert(prefix.clone(), state.clone())
                        .is_some_and(|old| old != state)
                    {
                        return Err(error(
                            Reason::StaleState,
                            path,
                            "Parent changed between inspections",
                        ));
                    }
                    continue;
                }
                if !create {
                    return Err(error(Reason::Filesystem, path, "Source parent is absent"));
                }
                creation_feasible(&fd, path)?;
                missing = true;
            }
            if !self.missing.iter().any(|p| p.path == prefix) {
                if self.missing.len() == 32 {
                    return Err(error(Reason::Limit, path, "More than 32 new directories"));
                }
                self.missing.push(NewDirectory {
                    path: prefix.clone(),
                    first_change: change,
                    mode: 0o700,
                });
            }
        }
        let key = if missing {
            format!("planned:{prefix}")
        } else {
            let m = fd.metadata().map_err(|e| io(path, e))?;
            format!("{}:{}", m.dev(), m.ino())
        };
        name_feasible(&fd, components.last().unwrap(), path)?;
        self.register(&key, components.last().unwrap(), path)?;
        if !self
            .targets
            .insert((key, folded(components.last().unwrap())))
        {
            return Err(error(
                Reason::Conflict,
                path,
                "Target aliases another source or destination on this volume",
            ));
        }
        Ok((fd, missing))
    }
    fn source(&mut self, path: &str, digest: &str, change: usize) -> Result<Source> {
        let prior = self
            .runtime
            .patch_reads
            .get(path)
            .filter(|r| r.sha256 == digest)
            .ok_or_else(|| {
                error(
                    Reason::ReadRequired,
                    path,
                    "Read this source in-session at the supplied hash",
                )
            })?;
        if self.runtime.seen.get(path).map(String::as_str) != Some(digest) {
            return Err(error(
                Reason::ReadRequired,
                path,
                "Read evidence has been invalidated",
            ));
        }
        let (parent, _) = self.parent(path, false, change)?;
        creation_feasible(&parent, path)?; // replacement/unlink needs writable searchable parent
        let name = path.rsplit('/').next().unwrap();
        lookup(&parent, name, path)?
            .ok_or_else(|| error(Reason::StaleState, path, "Source disappeared"))?;
        let file =
            openat(&parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0).map_err(|e| io(path, e))?;
        let m = file.metadata().map_err(|e| io(path, e))?;
        let state = Fingerprint::new(&m);
        if state != prior.state {
            return Err(error(
                Reason::StaleState,
                path,
                "Source identity or metadata changed since read",
            ));
        }
        if !m.is_file() || m.nlink() != 1 {
            return Err(error(
                Reason::UnsupportedMetadata,
                path,
                "Need regular single-link source",
            ));
        }
        let metadata = inspect_metadata(&file, &m);
        if prior.metadata.as_ref() != Some(&metadata) {
            return Err(error(
                Reason::StaleState,
                path,
                "Source metadata changed since read or read evidence unavailable",
            ));
        }
        if !self.sources.insert((m.dev(), m.ino())) {
            return Err(error(
                Reason::Conflict,
                path,
                "Source aliases another source",
            ));
        }
        let mut bytes = Vec::new();
        (&file)
            .take((FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| io(path, e))?;
        if bytes.len() > FILE_BYTES {
            return Err(error(Reason::Limit, path, "Source exceeds 64 KiB"));
        }
        if hash(&bytes) != digest
            || inspect_metadata(&file, &file.metadata().map_err(|e| io(path, e))?) != metadata
            || Fingerprint::new(&file.metadata().map_err(|e| io(path, e))?) != state
        {
            return Err(error(
                Reason::StaleState,
                path,
                "Source changed during inspection",
            ));
        }
        let content = String::from_utf8(bytes).map_err(|e| error(Reason::Text, path, e))?;
        validate_text(&content, path)?;
        Ok(Source {
            state,
            metadata,
            content,
            sha256: digest.into(),
        })
    }
    fn destination(&mut self, path: &str, change: usize) -> Result<()> {
        let (fd, missing) = self.parent(path, true, change)?;
        if !missing {
            if lookup(&fd, path.rsplit('/').next().unwrap(), path)?.is_some() {
                return Err(error(
                    Reason::Conflict,
                    path,
                    "Destination must be absent (including dangling symlinks)",
                ));
            }
            creation_feasible(&fd, path)?;
        }
        Ok(())
    }
}

fn plan_operation(change: &Change, before: Option<Source>) -> Result<Operation> {
    let after = match change {
        Change::Add { content, .. } => Some(content.clone()),
        Change::Delete { .. } => None,
        Change::Move { .. } => Some(before.as_ref().unwrap().content.clone()),
        Change::Update { .. } => Some(edited(
            &before.as_ref().unwrap().content,
            change.edits(),
            change.path(),
        )?),
    };
    let after_bytes = after.as_ref().map_or(0, String::len);
    let mut constraints = vec![match change {
        Change::Add { .. } => {
            "establish destination metadata/inheritance support before writes; create 0600 subject to umask"
        }
        Change::Update { .. } => {
            "establish metadata support before writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution"
        }
        Change::Delete { .. } => {
            "establish metadata support before unlink; revalidate source mode, flags, ACLs and xattrs or refuse execution"
        }
        Change::Move { .. } => {
            "establish metadata support before destination writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution; revalidate before source unlink"
        }
    }];
    if before.as_ref().is_some_and(|b| b.metadata.incomplete()) {
        constraints
            .push("metadata inspection incomplete; execution blocked until safety is established");
    }
    Ok(Operation {
        constraints,
        op: change.kind().into(),
        path: change.path().into(),
        to: if let Change::Move { to, .. } = change {
            Some(to.clone())
        } else {
            None
        },
        before_bytes: before.as_ref().map_or(0, |b| b.content.len()),
        after_bytes,
        after_sha256: after.as_ref().map(|s| hash(s.as_bytes())),
        mode: if after.is_some() {
            Some(before.as_ref().map_or(0o600, |b| b.state.mode & 0o777))
        } else {
            None
        },
        destination_absent: change.destination().is_some(),
        before,
        after,
        edits: change.edits().iter().map(|e| json!(e)).collect(),
    })
}

const PLAN_CONSTRAINTS: &[&str] = &[
    "not authorization; explicit consent to exact digest required",
    "revalidate whole plan after consent and each operation before mutation",
    "recheck anchored root, parent/source identities, metadata and hashes, and every absence",
    "exclusive parent creation (0700) and destination publication; no clobber",
    "sequential commits; record plan-owned directories and each committed substep; no rollback",
    "metadata support required before any write; safely preserve source attributes or refuse execution; add mode 0600 subject to umask",
];

impl Runtime {
    /// Read-only preparation, intentionally not dispatched or advertised to models.
    pub(crate) fn prepare_v2(&self, intent: &PatchIntent) -> Result<PatchPlan> {
        self.prepare_v2_checked(intent, || {})
    }
    fn prepare_v2_checked(
        &self,
        intent: &PatchIntent,
        between_inspections: impl FnOnce(),
    ) -> Result<PatchPlan> {
        let root = Fingerprint::new(&self.root_fd.metadata().map_err(|e| io("root", e))?);
        let mut inspection = Inspection {
            runtime: self,
            root,
            parents: BTreeMap::new(),
            missing: vec![],
            names: BTreeMap::new(),
            sources: HashSet::new(),
            targets: HashSet::new(),
        };
        inspection.root_unchanged()?;
        let mut operations = Vec::new();
        let mut total = 0;
        for (i, change) in intent.changes.iter().enumerate() {
            let before = change
                .digest()
                .map(|h| inspection.source(change.path(), h, i))
                .transpose()?;
            if let Some(dest) = change.destination() {
                inspection.destination(dest, i)?;
            }
            let operation = plan_operation(change, before)?;
            total += operation.after_bytes;
            if total > TOTAL_BYTES {
                return Err(error(
                    Reason::Limit,
                    "changes",
                    "Total resulting content exceeds 256 KiB",
                ));
            }
            operations.push(operation);
        }
        between_inspections();
        // Close the inspection window by independently reopening all paths. This
        // detects changes during preparation, not future races or authorization.
        inspection.root_unchanged()?;
        for (path, state) in &inspection.parents {
            let parts: Vec<_> = path.split('/').collect();
            let fd = self.directory(&parts).map_err(|e| io(path, e))?;
            if Fingerprint::new(&fd.metadata().map_err(|e| io(path, e))?) != *state {
                return Err(error(
                    Reason::StaleState,
                    path,
                    "Parent changed during planning",
                ));
            }
        }
        // Every absence is rechecked using the same no-follow traversal; none is
        // adopted when a previously missing parent appears.
        let mut recheck = Inspection {
            runtime: self,
            root: inspection.root.clone(),
            parents: BTreeMap::new(),
            missing: vec![],
            names: BTreeMap::new(),
            sources: HashSet::new(),
            targets: HashSet::new(),
        };
        for (i, change) in intent.changes.iter().enumerate() {
            if let Some(digest) = change.digest() {
                let fresh = recheck.source(change.path(), digest, i)?;
                if operations[i].before.as_ref() != Some(&fresh) {
                    return Err(error(
                        Reason::StaleState,
                        change.path(),
                        "Source changed between inspections",
                    ));
                }
            }
            if let Some(dest) = change.destination() {
                recheck.destination(dest, i)?;
            }
        }
        recheck.root_unchanged()?;
        if recheck.parents != inspection.parents {
            return Err(error(
                Reason::StaleState,
                "parents",
                "Parent changed between inspections",
            ));
        }
        if recheck.missing.iter().map(|d| &d.path).collect::<Vec<_>>()
            != inspection
                .missing
                .iter()
                .map(|d| &d.path)
                .collect::<Vec<_>>()
        {
            return Err(error(
                Reason::StaleState,
                "parents",
                "Previously absent parent appeared",
            ));
        }
        Ok(PatchPlan {
            version: 2,
            root: inspection.root,
            parents: inspection.parents,
            new_directories: inspection.missing,
            operations,
            total_result_bytes: total,
            constraints: PLAN_CONSTRAINTS.to_vec(),
        })
    }
}

// Inspect actual names as well as fstatat identity. This catches canonical/case
// aliases on the host volume, and conservatively refuses them on sensitive ones.
fn lookup(parent: &File, name: &str, location: &str) -> Result<Option<Identity>> {
    let names = directory_names(parent, location)?;
    let wanted = folded(name);
    for actual in names {
        if folded(&actual) == wanted && actual != name {
            // Canonical spellings returned by HFS/APFS can differ from input.
            // Require the exact same bytes in the plan to avoid implicit aliases.
            return Err(error(
                Reason::Conflict,
                location,
                format!("Filesystem name aliases {actual:?}"),
            ));
        }
    }
    component_identity(parent, name, location)
}
fn component_identity(parent: &File, name: &str, location: &str) -> Result<Option<Identity>> {
    let c = CString::new(name).map_err(|e| io(location, e))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid descriptor, component string and output storage; no symlink following.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        let st = unsafe { stat.assume_init() };
        #[allow(clippy::unnecessary_cast)]
        return Ok(Some(Identity {
            device: st.st_dev as u64,
            inode: st.st_ino,
        }));
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(io(location, e))
    }
}
fn directory_names(parent: &File, location: &str) -> Result<Vec<String>> {
    let fd = openat(parent, ".", libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(|e| io(location, e))?
        .into_raw_fd();
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(io(location, std::io::Error::last_os_error()));
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
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(guard.0) };
        if entry.is_null() {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                return Err(io(location, e));
            }
            break;
        }
        let raw = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = raw.to_str().map_err(|_| {
            error(
                Reason::Path,
                location,
                "Cannot safely compare non-UTF8 directory entry",
            )
        })?;
        if name != "." && name != ".." {
            names.push(name.into());
        }
        // Bound untrusted directory scanning; inability to establish collision
        // absence is a refusal, never an assumed success.
        if names.len() > 10000 {
            return Err(error(
                Reason::Limit,
                location,
                "Parent listing exceeds 10000 names",
            ));
        }
    }
    Ok(names)
}
fn name_feasible(parent: &File, name: &str, location: &str) -> Result<()> {
    // Let the actual filesystem validate the component with a read-only lookup.
    // In particular, macOS NAME_MAX is not a UTF-8 byte ceiling. Both an existing
    // entry and ENOENT establish that the name passed lookup validation; all other
    // errors (including ENAMETOOLONG) fail closed. For planned parents, use the
    // nearest existing ancestor on the same volume; execution must revalidate.
    component_identity(parent, name, location).map(|_| ())
}
fn creation_feasible(parent: &File, location: &str) -> Result<()> {
    safe_directory(parent, location)?;
    let dot = c".";
    if unsafe {
        libc::faccessat(
            parent.as_raw_fd(),
            dot.as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    } != 0
    {
        return Err(io(
            location,
            "Parent is not writable/searchable by this user",
        ));
    }
    let mut fs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(parent.as_raw_fd(), fs.as_mut_ptr()) } != 0 {
        return Err(io(location, std::io::Error::last_os_error()));
    }
    if unsafe { fs.assume_init() }.f_flag & libc::ST_RDONLY != 0 {
        return Err(io(location, "Read-only filesystem"));
    }
    Ok(())
}
fn safe_directory(file: &File, location: &str) -> Result<()> {
    let m = file.metadata().map_err(|e| io(location, e))?;
    if !m.is_dir() {
        return Err(io(location, "Expected directory"));
    }
    if m.mode() & 0o7000 != 0 {
        return Err(error(
            Reason::UnsupportedMetadata,
            location,
            "Special parent mode bits can change creation or unlink semantics",
        ));
    }
    // ACL inheritance and immutable flags can invalidate predicted create modes.
    safe_parent_metadata(file, location)
}
// Observations are evidence for a future executor, never a grant to mutate.
// Keep values out of previews/plans; hashes detect same-name value changes.
const METADATA_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
enum Observation<T> {
    Observed(T),
    Unavailable(String),
}
impl<T> Observation<T> {
    fn unavailable(message: &str) -> Self {
        Self::Unavailable(message.into())
    }
    fn failed(operation: &str) -> Self {
        Self::Unavailable(format!(
            "{operation}: errno {}",
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        ))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BlobState {
    bytes: usize,
    sha256: String,
}
impl BlobState {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len(),
            sha256: hash(bytes),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct XattrState {
    name: Vec<u8>,
    value: BlobState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct MetadataObservation {
    special_mode_bits: u32,
    flags: Observation<u32>,
    xattrs: Observation<Vec<XattrState>>,
    acl: Observation<BlobState>,
}
impl MetadataObservation {
    fn incomplete(&self) -> bool {
        matches!(self.flags, Observation::Unavailable(_))
            || matches!(self.xattrs, Observation::Unavailable(_))
            || matches!(self.acl, Observation::Unavailable(_))
    }
    fn summary(&self) -> String {
        let flags = match &self.flags {
            Observation::Observed(n) => format!("observed {n}"),
            Observation::Unavailable(_) => "inspection unavailable".into(),
        };
        let xattrs = match &self.xattrs {
            Observation::Observed(v) => format!("observed {}", v.len()),
            Observation::Unavailable(_) => "inspection unavailable".into(),
        };
        let acl = match &self.acl {
            Observation::Observed(v) => format!("observed {} bytes", v.bytes),
            Observation::Unavailable(_) => "inspection unavailable".into(),
        };
        format!(
            "special mode {:#o}; flags {flags}; xattrs {xattrs}; ACL {acl}",
            self.special_mode_bits
        )
    }
}
fn inspect_metadata(file: &File, m: &Metadata) -> MetadataObservation {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        MetadataObservation {
            special_mode_bits: m.mode() & 0o7000,
            flags: Observation::Observed(m.st_flags()),
            xattrs: inspect_xattrs(file.as_raw_fd()),
            acl: inspect_acl(file.as_raw_fd()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        MetadataObservation {
            special_mode_bits: m.mode() & 0o7000,
            flags: Observation::unavailable("File flags inspection unsupported on this platform"),
            xattrs: Observation::unavailable("Xattr inspection unsupported on this platform"),
            acl: Observation::unavailable("ACL inspection unsupported on this platform"),
        }
    }
}
#[cfg(target_os = "macos")]
fn inspect_xattrs(fd: std::os::fd::RawFd) -> Observation<Vec<XattrState>> {
    // Every allocation and total value read is bounded. No retries adopt raced
    // state; a failed inspection is retained as an explicit execution constraint.
    let n = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    if n < 0 {
        return Observation::failed("flistxattr");
    }
    if n as usize > 65536 {
        return Observation::unavailable("Xattr names exceed 64 KiB inspection bound");
    }
    let mut names = vec![0u8; n as usize];
    if unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0) } != n {
        return Observation::unavailable("Xattr listing changed or could not be inspected");
    }
    if !names.is_empty() && names.last() != Some(&0) {
        return Observation::unavailable("Malformed xattr listing");
    }
    let mut attributes = Vec::new();
    let mut total = 0;
    for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let c = CString::new(name).expect("split xattr name");
        let size = unsafe { libc::fgetxattr(fd, c.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        if size < 0 {
            return Observation::failed("fgetxattr size");
        }
        total += size as usize;
        if total > METADATA_BYTES {
            return Observation::unavailable("Xattr values exceed 1 MiB inspection bound");
        }
        let mut value = vec![0u8; size as usize];
        if unsafe { libc::fgetxattr(fd, c.as_ptr(), value.as_mut_ptr().cast(), value.len(), 0, 0) }
            != size
        {
            return Observation::unavailable("Xattr value changed or could not be inspected");
        }
        attributes.push(XattrState {
            name: name.to_vec(),
            value: BlobState::new(&value),
        });
    }
    attributes.sort_by(|a, b| a.name.cmp(&b.name));
    Observation::Observed(attributes)
}
#[cfg(target_os = "macos")]
fn inspect_acl(fd: std::os::fd::RawFd) -> Observation<BlobState> {
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_EXTENDED_SECURITY,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // Aligned output storage: length plus attrreference(offset, length), payload.
    let mut buffer = vec![0u32; 16384];
    if unsafe {
        libc::fgetattrlist(
            fd,
            (&mut attrs as *mut libc::attrlist).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len() * 4,
            0,
        )
    } != 0
    {
        return Observation::failed("fgetattrlist ACL");
    }
    let length = buffer[0] as usize;
    let offset = 4i64 + i64::from(buffer[1] as i32);
    let bytes = buffer[2] as usize;
    if length < 12
        || length > buffer.len() * 4
        || (bytes != 0 && (offset < 12 || offset as usize + bytes > length))
    {
        return Observation::unavailable("Invalid or oversized ACL response");
    }
    let raw = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length) };
    let payload = if bytes == 0 {
        &[]
    } else {
        &raw[offset as usize..offset as usize + bytes]
    };
    Observation::Observed(BlobState::new(payload))
}
fn safe_parent_metadata(file: &File, location: &str) -> Result<()> {
    // Existing publication parents retain the conservative feasibility policy.
    // Source inspection above records observations without executor eligibility.
    let m = file.metadata().map_err(|e| io(location, e))?;
    let observed = inspect_metadata(file, &m);
    if observed.incomplete()
        || !matches!(observed.flags, Observation::Observed(0))
        || !matches!(observed.acl, Observation::Observed(ref acl) if acl.bytes == 0)
    {
        return Err(error(
            Reason::UnsupportedMetadata,
            location,
            "Parent metadata safety cannot be established",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "patch_tests.rs"]
mod tests;
