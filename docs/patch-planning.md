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
[contract](tool-architecture.md#format-choice) are accepted. A move has exactly
`op`, `path`, `to` and `sha256`, preserves the source bytes/hash and requires an
absent destination. `edits` is rejected on a move even when empty; only updates
accept the closed nested edit shape. Duplicate fields are rejected, hashes contain
exactly 64 lowercase hex characters, and no malformed batch is repaired,
truncated or partially accepted.
Unknown fields, missing fields, wrong types, invalid Unicode escapes and unknown
operations fail deserialization. Pure validation checks text, paths, edit shapes,
case/canonical-equivalent targets and file/directory prefix dependencies.

`Runtime::prepare_v2(&PatchIntent) -> Result<PatchPlan, PatchError>` performs
read-only filesystem inspection and exact original-byte edit matching. It takes
`&self`, records no events and changes neither the legacy read map nor the modified
file ledger. `read_file` now also records private source identity/metadata evidence
alongside its existing hash, without changing its result or acceptance policy.
An externally populated legacy `seen` map cannot manufacture v2 read evidence.
A best-effort descriptor capture adds metadata observations to the read evidence;
if its identity changes or reopening fails, v2 requires another read while the
legacy result remains unchanged. Re-read after legacy mutations before attempting
v2 preparation.

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
| Edits per source | 1–16 for update only; moves reject the field |
| Source or resulting file | 64 KiB UTF-8 bytes |
| Total resulting content (unchanged move destinations included; deletions count zero) | 256 KiB |
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
current bytes/hash and metadata observations. Sources must be regular single-link
files. Reads use nonblocking opens to avoid FIFO hangs and bounded reads. All
path, identity, source-type and destination-absence checks remain fail-closed.

On macOS, descriptor-based `flistxattr`/`fgetxattr` and `fgetattrlist` record source
xattrs and ACLs. Each metadata aspect has an `observed` or `unavailable` status:
an observed empty collection/payload establishes absence, a nonempty observation
establishes presence, and an unavailable observation records inspection failure
or missing platform support. File flags and special mode bits are also recorded.
Ordinary OS provenance, other xattrs, ACLs, flags and special source modes do not
prevent preview planning. They establish execution constraints, not permission to
mutate. Xattr names use exact byte arrays, sorted by name; values and ACL payloads
are represented by byte counts and SHA-256 hashes, never raw attribute values.
Same-name, same-length value changes therefore change the observation and digest.

Metadata inspection is bounded to 64 KiB of xattr names, 1 MiB of total xattr
values and a 64 KiB ACL response. Exceeded bounds and failed calls become explicit
unavailable observations, never assumed absence. Stable unavailable source
observations allow a preview with an additional constraint blocking execution
until safety is established. Changed observations invalidate prior read evidence
or the first inspection rather than silently refreshing it.

Existing publication parents retain conservative feasibility checks: special mode
bits, ACLs, flags or unavailable metadata inspection are refused. Parent xattrs
are inspected for availability and retained; they need not be absent. Effective
write/search access and read-only mount flags are checked on existing publication
parents or the nearest existing parent of planned directories. These establish
feasibility, not available disk space or a guarantee of later success. Milestone 3
must independently inspect parent metadata/inheritance before writing as well.

MacOS is the evaluated platform. Other platforms record unavailable source
metadata support and cannot establish the required parent metadata feasibility;
this implementation does not claim native planning or command sandbox support
there. No sandbox permissions changed.

Preparation independently inspects every source/destination a second time,
reopens recorded parents and checks the named root against the anchored descriptor.
A new parent appearing between passes is stale state, never implicitly adopted.
These checks detect changes during inspection but cannot isolate a hostile
same-user process or make a cross-file transaction. Ordinary reads can update
kernel-managed access times depending on mount policy; the planner never calls
metadata setters. Snapshot tests include directory and hidden-entry names, bytes,
identity, modes, ownership, links, size, modification/change timestamps, file
flags, xattr names/values and ACL payloads, excluding access times. Tests also
compare the externally changed tree immediately after an injected race with the
tree after refusal to distinguish the race's effects from planner mutations.

## Immutable plan and inspection

`PatchPlan` has private fields and no mutation accessors. `inspect()` returns an
owned JSON copy. It includes version, anchored root fingerprint, every existing
parent fingerprint, each absent new parent's path/mode/first owning change,
ordered exact operations/edits, source bytes/hash/fingerprint, resulting
bytes/hash, before/after byte counts, destination-absence expectations, intended
ordinary file modes, source metadata observations and per-operation executor
constraints. Absence is represented by null before/after content for add/delete
and explicit destination/parent states.

`digest()` hashes the deterministic JSON serialization of the entire plan, without
including the digest itself. Array order follows the request and parent first use;
existing-parent maps have sorted keys. Refreshing a plan can change its digest even
if the textual diff is unchanged, because expected metadata is part of consent.

`preview()` first lists **all** operations, source/destination paths, byte counts
and planned directory creations, with source metadata summaries and
operation-specific execution constraints. An unchanged move shows both paths,
its unchanged-content SHA-256 and byte count, and has no content diff. Only then
does the preview spend the shared 8,192 escaped-character content budget on unified
add/update/delete diffs. Truncation is explicit and never removes summary paths.
Path spelling is quoted;
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
  status, bytes, hash, ordinary/special modes, flags, ACL and xattr observations
  (including exact names/value hashes and inspection availability).
- Metadata support **before any target write**: independently establish safe
  inspection and handling for source attributes and destination/parent inheritance.
  Preserve attributes safely for update/move or refuse execution with an explicit
  unsupported result. A preview carrying attributes or an unavailable observation
  is never permission to discard them or to bypass metadata safety. Delete must
  establish safe unlink semantics and revalidate its metadata constraints too.
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

The 2026-11-11 revision applies the approved Issue #20 contract: moves preserve
content, and read-only source inspection records metadata observations separately
from executor eligibility. This supersedes the previous xattr-free-volume
prerequisite and its five printed native skips; no human result is substituted.
The checks below ran in the assigned Apple Silicon macOS worktree. Tests live in
[patch_tests.rs](../src/patch_tests.rs). The native volume demonstrated both case
and canonical-Unicode aliasing. Ordinary OS provenance remains in source fixtures;
tests no longer attempt to remove it or fall back to fabricated mixed plans.

| Check | Actual result |
| --- | --- |
| Current Issue and discussion | Pass: fetched Issue #20 and its discussion; the current Issue explicitly supersedes the old xattr-free evidence request. |
| `cargo test --locked --all-targets` | Pass: 88 tests, zero failures/ignored tests; existing runtime, agent, CLI, provider, launcher, creation and real macOS sandbox regressions executed. |
| `cargo test --locked --lib runtime::patch -- --nocapture` | Pass: 18 planner test groups. Native mixed-source success, source byte/inode/mode races and aggregate unchanged-move boundary all executed, with no skipped subcases. Native ACL and xattr value observation/change cases also passed. |
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass. |
| `cargo run --locked --example check_docs` | Pass: 15 Markdown documents, zero errors. |
| `git diff --check` and staged equivalent | Pass. |

| Acceptance | Implementation and evidence |
| --- | --- |
| AC1 | Closed four-field moves, rejection of empty/nonempty/null move edits; strict parser shape/type/version/hash/Unicode and argument-boundary coverage. |
| AC2 | Strict path/text/edit ceilings and original-byte matching; native unchanged moves accept exactly 256 KiB total and reject 320 KiB. |
| AC3 | Whole-batch source/destination conflicts, including updates of a moved source or destination; native namespace aliases, missing-parent ordering and component feasibility. Native multibyte source moves now plan successfully. |
| AC4 | Anchored no-follow traversal and prior read evidence; metadata presence/inspection-failure distinction, native source special mode/flags/ACL/xattrs, same-length xattr value and ACL change detection, fail-closed identity/path/destination checks. |
| AC5 | Immutable ordered exact plans and native mixed plan/preview goldens; unchanged move paths/hash/bytes, metadata summaries and operation-specific constraints remain visible when content is truncated. |
| AC6 | Native success/failure/race snapshots include hidden entries, directories, identities, flags, xattr names/values and ACL payloads; unchanged legacy regressions pass. Kernel-managed atime is excluded as explained above. |
| AC7 | All five formerly skipped native subcases executed successfully. Byte/inode/mode and ACL/xattr races detect stale state; native ACL/xattr/flag observations, case/Unicode aliases and multibyte component limits also pass. |
| AC8 | Revised contract, plan/preview interface and metadata handoff documented here and in the architecture decision; v2 remains unavailable to models. |

Skipped mandatory native cases: none. Non-macOS validation, separate release/MSRV
builds and optional fuzzing: not run in this revision. No Ollama model trials are
required for this internal milestone. Required post-merge verification: none.
