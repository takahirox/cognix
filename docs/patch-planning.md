# Internal v2 patch preparation (Issue #20)

Milestone 2 of the [tool decision](tool-architecture.md#ordered-implementation-roadmap)
is implemented in [patch.rs](../src/patch.rs), an internal child of the existing
runtime. V2 is **not available to models**, has no executor, and changes no tool
schema, dispatch, CLI option, approval default or sandbox permission. Preparation
cannot approve, stage, publish, unlink, rename or create anything. The future
executor must use the [shared action lifecycle](implementation.md#shared-action-lifecycle),
not a second approval or evidence runtime. Both preparations use the shared pure
diff builder in `action.rs`; v2 does not invoke `ActionPlan::approve`.

## Interfaces and errors

`PatchIntent::parse(&[u8]) -> Result<PatchIntent, PatchError>` is pure. It bounds
raw arguments before decoding UTF-8/JSON and produces private, typed intent.
Only version 2 and the four closed operation shapes in the
[contract](tool-architecture.md#format-choice) are accepted. Nested edits are
closed, duplicate fields are rejected, hashes contain exactly 64 lowercase hex
characters, and no malformed batch is repaired, truncated or partially accepted.
Unknown fields, missing fields, wrong types, invalid Unicode escapes and unknown
operations fail deserialization. Pure validation checks text, paths, edit shapes,
case/canonical-equivalent targets and file/directory prefix dependencies.

`Runtime::prepare_v2(&PatchIntent) -> Result<PatchPlan, PatchError>` performs
read-only filesystem inspection and exact original-byte edit matching. It takes
`&self`, records no events and changes neither the legacy read map nor the modified
file ledger. `read_file` now also records private source identity/metadata evidence
alongside its existing hash, without changing its result or acceptance policy.
An externally populated legacy `seen` map cannot manufacture v2 read evidence.
Re-read a file after legacy mutations before attempting v2 preparation.

Errors expose a `reason`, `location` (field/index or affected path) and sanitized
`detail`. Reasons distinguish JSON syntax, closed shape/type, version, limit,
path, hash, text, edit, conflict, read-required, stale-state, unsupported-metadata
and filesystem failures. Deserializer errors include line/column and expected
fields/types. Locations are structured untrusted data and must be escaped if
displayed. The module remains crate-internal and deliberately allows unused
preparation interfaces until the executor milestone connects them.

## Limits and matching

| Resource | Ceiling |
| --- | --- |
| Raw serialized JSON arguments | 1 MiB |
| Changes | 1–8 |
| Edits per source | 1–16 for update; 0–16 for move |
| Source or resulting file | 64 KiB UTF-8 bytes |
| Total resulting content (deletions count zero) | 256 KiB |
| Path | 4,096 UTF-8 bytes; 32 components |
| New parent directories | 32, deduplicated in first-use ancestor order |
| Content diff display | 8,192 escaped characters across the batch |
| Existing directory collision scan | 10,000 names; refuse larger/undecodable listings |

Paths reject empty, absolute, trailing-slash, empty/`.`/`..` components, NUL and
case-insensitive `.git`. There is no expansion. Components must also fit the
host filesystem's name rules: descriptor-relative no-follow lookups validate each
component, accepting an existing entry or `ENOENT` and refusing all other errors,
including `ENAMETOOLONG`. The reported `NAME_MAX` is not treated as a UTF-8 byte
ceiling, which would reject valid macOS multibyte names. For missing parents,
component validation uses the nearest existing ancestor's volume; the executor
must revalidate against the actual parents before mutation. Text cannot contain
NUL. Every nonempty old span must match exactly once, including overlapping
occurrences. Matched intervals cannot overlap, and replacements use original
bytes simultaneously, in source order. Neither newline normalization nor fuzzy
matching occurs. Individual and combined no-ops are refused. Empty files can be
added/deleted or moved unchanged; an update still needs an existing unique anchor.

## Filesystem inspection and assumptions

The planner reuses the canonical anchored root, `openat` with `O_NOFOLLOW`, and
descriptor-relative directory traversal. `fstatat(AT_SYMLINK_NOFOLLOW)` detects
all existing destinations, including dangling symlinks. Existing entries are
also enumerated through directory descriptors. Unicode normalization and full
case folding conservatively reject case/canonical aliases even on sensitive
volumes; inode/device and parent/child namespace comparisons additionally catch
aliases resolved by the actual volume. Ambiguous alternate spellings of shared
parents are refused. Swaps, cycles, case-only moves and dependencies on any other
operation's file output fail before a plan is returned. Only missing add/move
parents can be shared. No filesystem probe creates a name to test equivalence.

Sources must have prior in-session evidence at the supplied hash. Inspection
compares identity, mode, uid/gid, link count, size, mtime/ctime with nanoseconds,
and current bytes/hash. Sources must be regular single-link files without special
mode bits. Reads use nonblocking opens to avoid FIFO hangs and bounded reads.
On macOS, descriptor-based xattr listing and `fgetattrlist` extended-security
inspection reject any source xattr/ACL, including empty ACL payloads, and file
flags. Detection errors fail closed. Parent ACLs/flags and special mode bits are
also refused to avoid unplanned inheritance or creation restrictions. Unmodified parent xattrs are
inspected for availability but are retained and need not be absent. Effective
write/search access and read-only mount flags are checked on existing publication
parents or the nearest existing parent of planned directories. These checks
establish feasibility, not available disk space or a guarantee of later success.

Linux has an unvalidated conservative xattr/ACL check (including refusal of parent
xattrs); other platforms cannot establish metadata safety and refuse it. This is
not a new supported command sandbox. MacOS is the evaluated platform.

Preparation independently inspects every source/destination a second time,
reopens recorded parents and checks the named root against the anchored descriptor.
A new parent appearing between passes is stale state, never implicitly adopted.
These checks detect changes during inspection but cannot isolate a hostile
same-user process or make a cross-file transaction. Ordinary reads can update
kernel-managed access times depending on mount policy; the planner never calls
metadata setters. Snapshot tests include directory and hidden-entry names, bytes,
identity, modes, ownership, links, size and modification/change timestamps, excluding
access times. The exact-byte pure builder can also be tested against fixed source
snapshots independently of platform metadata availability.

## Immutable plan and inspection

`PatchPlan` has private fields and no mutation accessors. `inspect()` returns an
owned JSON copy. It includes version, anchored root fingerprint, every existing
parent fingerprint, each absent new parent's path/mode/first owning change,
ordered exact operations/edits, source bytes/hash/fingerprint, resulting
bytes/hash, before/after byte counts, destination-absence expectations, intended
ordinary file modes and explicit executor constraints. Absence is represented by
null before/after content for add/delete and explicit destination/parent states.

`digest()` hashes the deterministic JSON serialization of the entire plan, without
including the digest itself. Array order follows the request and parent first use;
existing-parent maps have sorted keys. Refreshing a plan can change its digest even
if the textual diff is unchanged, because expected metadata is part of consent.

`preview()` first lists **all** operations, source/destination paths, byte counts
and planned directory creations. Only then does it spend the shared 8,192 escaped
character content budget on unified add/update/delete/move-with-edit diffs.
Truncation is explicit and never removes summary paths. Path spelling is quoted;
controls, bidi and invisible characters are escaped, with real newlines retained
only as diff line separators. Summary size is bounded by the path/change/directory
ceilings separately from the content budget. `complete_escaped()` provides the
complete deterministic escaped plan, without truncation or raw terminal controls;
`inspect()` retains exact unescaped bytes as structured data. No file is written
for inspection. The future CLI must offer full-plan inspection before accepting
consent, using these accessors; this milestone deliberately adds no CLI option.

## Executor handoff

A returned plan is **not authorization** and has no execution method. Milestone 3
must, after explicit consent to the exact preview/digest, independently revalidate
the whole plan and, immediately before each mutation, all of the following:

- The anchored root and every existing parent's identity and expected state.
- Each source's fresh in-session evidence, inode/device, regular/single-link
  status, ordinary mode, unsupported metadata absence, bytes and hash.
- All destination absences using no-follow lookup, each new-parent absence and
  namespace conflicts/creation feasibility. An externally created parent is stale.
- Exclusive creation of plan-owned directories, mode 0700 subject to umask, in
  ancestor-first first-use order. Retain actual identities for later operations.
- Exclusive no-clobber destination publication, source-preserving staging, update
  replacement and move publication-then-source-unlink substeps. Recheck before
  each substep, including the source before a move unlink.
- Cancellation and unchanged exact consent. A refreshed or edited plan needs new
  approval. Record every visible substep in the existing ledger before any next
  fallible step; preserve partial results, user edits and owned-artifact identity.

A failure can leave sequential earlier effects. There is no batch atomicity,
automatic rollback, Git reset, durable crash recovery or permission expansion.
After its own commits the executor must track proven owned effects rather than
blindly comparing initial directory timestamps with timestamps changed by its
own publications. Directory identities remain mandatory; external state must
never be adopted as an owned effect.

## Validation record

Validation took place on the assigned macOS worktree on 2026-11-10. Tests live in
[patch_tests.rs](../src/patch_tests.rs). Native volume probes establish both
case-insensitive and canonical-Unicode alias behavior rather than assuming it.

The host automatically attaches `com.apple.provenance` to disposable files and
retains it even when descriptor removal reports success. Production correctly
refuses these sources. The mixed golden therefore exercises the pure operation
builder using fixed snapshots on this host, while asserting the native refusal
and unchanged tree. The test reports this explicitly; it does not suppress
metadata detection. On a volume permitting ordinary xattr-free files it also runs
the full native mixed success path.

Native mixed-source success, three source-specific between-inspection races
(bytes, inode and metadata), and the aggregate move-content limit boundary are
**Skipped** here for that concrete host limitation. These remain mandatory native
validation gaps for AC7 before claiming full Issue completion. Source identity/hash
changes before inspection, add-parent/destination/root races, alias refusals,
read-only success/failure snapshots and real ACL/xattr/flags refusals are exercised.
The pure mixed plan/preview golden and pure edit limits remain covered. Cargo's
aggregate pass count does not convert the printed native skips into passes.

The initial implementation checks were:

| Check | Actual result |
| --- | --- |
| Current Issue and comments | Pass: fetched Issue #20 and its discussion (no comments) through the GitHub connector. |
| `cargo test --locked --all-targets` | Pass: 84 Rust tests, including the existing runtime, agent, CLI, provider, launcher, creation and real macOS sandbox suites. Five native v2 subcases remain skipped as described above; zero ignored test functions does not mean those subcases ran. |
| `cargo test --locked --lib runtime::patch -- --nocapture` | Pass: 14 test groups, with actual case/canonical alias observations and explicit native-skip messages. |
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass. |
| `cargo run --locked --example check_docs` | Pass: 15 Markdown documents, zero errors. |
| `git diff --check` and staged equivalent | Pass. |
| `cargo build --locked --release` | Pass on rustc 1.94.0, Apple Silicon macOS. The advertised 1.87 minimum was respected in syntax; a 1.87 toolchain was not installed or separately tested. |

| Acceptance | Implementation and evidence |
| --- | --- |
| AC1 | Closed derived records, bounded byte parser and structured errors; malformed/duplicate/missing/extra/type/version/hash/Unicode tests. |
| AC2 | Parser ceilings and strict paths; original-byte unique-span/overlap/no-op/result-boundary tests; native new-directory and filesystem-name ceilings. Native aggregate move boundary remains skipped. |
| AC3 | Whole-batch target/prefix checks, actual descriptor namespace identities, full case folding/canonical normalization, real-volume alias fixtures, shared parent first-use ordering. |
| AC4 | Anchored no-follow traversal, source read fingerprints/hashes, absence and feasibility checks, native xattr/ACL/flags/special-mode refusals. Post-inspection source race evidence remains skipped. |
| AC5 | Ordered immutable exact plan, digest, escaped complete inspection and bounded previews; pure mixed golden and deterministic real add plans. |
| AC6 | Successful/failed native before/after snapshots including hidden entries, directory identity, flags, xattr names and ACL payloads; unchanged legacy regression suites. Kernel-managed atime is excluded as explained above. |
| AC7 | **Incomplete native validation**: five subcases require an xattr-free source volume. All other executed checks above passed; full Issue completion remains blocked by these mandatory gaps. |
| AC8 | This guide, implementation/decision links and README boundary; explicit executor revalidation and consent requirements. |

No Ollama model trials are required for this non-advertised milestone. Required
post-merge verification: none required.

### Review revision: native component validation

The 2026-11-10 revision replaces the UTF-8 byte comparison against `NAME_MAX`
with read-only native component lookup. New macOS regressions establish actual
creation/lookup behavior for 100 and 255 Japanese characters, 200 and 255 accented
characters, and 127 supplementary Unicode characters. They exercise existing
source component feasibility and successful add planning through existing and
missing multibyte parents, with unchanged tree/metadata snapshots. Native creation
and planning both reject 256 ASCII/Japanese/accented characters and 128
supplementary characters, including invalid components below planned parents.

Pass on the revised tree: `cargo test --locked --all-targets` (86 tests, including
the existing native runtime/agent/CLI/sandbox regressions), focused planner tests
(16 groups with `--nocapture`), formatting, Clippy with warnings denied, the
documentation checker (15 documents, zero errors), and whitespace checks. The
same five native subcases above still print explicit provenance-related skips;
these new name tests do not establish full native source-planning success or
resolve the outstanding validation question. Production metadata checks remain
enabled. The PR description uses a non-closing Issue reference while this
mandatory evidence remains incomplete.
