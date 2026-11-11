# Tool architecture decision (Issue #16)

Status: proposed architecture decision for review; implementation is deferred to
the milestones below. This decision resolves the design scope of
[Issue #16](https://github.com/takahirox/cognix/issues/16), not its future runtime
work. Existing schemas, approvals and sandbox behavior remain unchanged.

Milestone 2's [internal parser and read-only planner](patch-planning.md) is now
implemented. V2 remains unavailable to models and has no executor or CLI mode;
the validation record in that guide includes native planning with source metadata
observations and execution constraints. Later milestones remain future work.

## Decision and boundaries

Prioritize a small set of composable tools: general `apply_patch` for reviewed
filesystem changes and policy-controlled `exec_command` for development commands.
Keep `read_file` and `list_files` while they provide reliable bounded discovery
and content hashes without launching a process. A two-tool mutation/execution
core does not require removing these safer read interfaces. Do not add a separate
model-facing tool for each filesystem operation by default. A specialized tool
needs evidence of a reliability or security benefit that the shared tools cannot
provide at comparable complexity.

Capability describes what a backend can do; authorization describes what this
session and this exact approved action may do. Neither a tool name nor user
approval grants filesystem, network or process permissions beyond the backend's
enforced boundary. No unrestricted shell is authorized by this decision.

The general editing/command pattern is motivated by the coding-agent reference
in the Issue. This is an independent Cognix contract, not a claim that Codex has
the same approval, patch, atomicity or sandbox semantics. Comparing current
Codex source is optional and was not performed for this decision.

Desktop UI, MCP, multiple agents, Linux sandbox implementation, automatic Git
commits and cloud inference are outside this work. Normal inference remains
local Ollama, without automatic model/provider fallback or downloads.

## Current implementation and its limits

This analysis applies to the source inherited from `ff984a9` (the runtime is
unchanged by this decision). Canonical current behavior is documented in the
[implementation guide](implementation.md); the
[MVP specification](mvp.md#f4--minimal-tools) preserves the original four-tool
scope and the later creation extension.

| Tool in [schema.rs](../src/schema.rs) | Actual capability | Constraint and evidence |
| --- | --- | --- |
| `list_files(path)` | Sorted, single-directory listing; `.` selects root. | Omits symlinks and case-insensitive `.git`; at most 500 entries and the output byte limit. No recursive search. |
| `read_file(path)` | Reads a regular, single-hard-link UTF-8 file without NULs, at most 64 KiB. | Returns content and SHA-256; records the normalized path/hash in the runtime's seen map. No binary or large-file editing. |
| `apply_patch(path, sha256, old_text, new_text)` | Replaces exactly one unique nonempty span in a previously read existing file. | Rejects stale hashes, ambiguous/no-op spans and oversized results. Diff and exact replacement precede approval; same-directory atomic replacement preserves ordinary mode bits, but not ACLs/xattrs. No add/delete/move/multiple-file operation. |
| `create_file(path, content)` | Publishes one new regular file with complete UTF-8 content, at most 64 KiB, including empty content. | Existing parents only; no overwrite. Preview/approval and descriptor-relative exclusive publication; mode 0600 subject to umask. Read it before subsequently patching. |
| `run_command(argv)` | Executes one exact startup-allowlisted argv after separate approval. | Absolute installed runtime executable, no shell expansion, project read-only, scratch writes only, no network or descendants, `.git` unreadable. Commands are disabled by default. |

[runtime.rs](../src/runtime.rs) opens a canonical root once and traverses file
paths with directory descriptors and `O_NOFOLLOW`. Absolute paths, parent
traversal, Git metadata components (including case aliases), symlinks and
nonregular/multiply-linked file targets are refused. Roots, parents, identities
and contents are rechecked around approval/publication. Creation also rejects
trailing slashes and all existing destinations. Creation and replacement have
different publication primitives but duplicate preview, approval, cancellation,
stale-boundary checks, temporary ownership and bookkeeping concerns. New narrow
mutation tools would multiply this policy surface. Share orchestration while
retaining operation-specific safe syscalls.

The file boundary does not isolate a hostile same-user process moving directories
between checks and syscalls. Cancellation does not undo committed edits or user
changes; there is no automatic Git reset or rollback. Root/parent identity checks
are useful defense, not a filesystem transaction or an OS sandbox.

[commands.rs](../src/commands.rs) uses macOS Seatbelt via `sandbox-exec`, with
deny-by-default rules. Runtime/project reads and scratch writes are permitted;
outside data, network, project writes, Git data, process spawning/forking and Mach
service lookups are denied. The environment is rebuilt, inherited descriptors
above stderr are closed on exec, stdin is null, and launch failure never retries
unsandboxed. After approval a bounded project scan rejects hard-link aliases
even when absent from argv. Unsupported platforms return `not_run` before
approval/launch. See [current isolation details](implementation.md#verification-policy-and-isolation).

[agent.rs](../src/agent.rs) executes calls sequentially in response order, with
16 model iterations by default and at most eight calls per response. Malformed
envelopes/duplicate call IDs stop a batch before effects; ordinary argument/tool
errors return to the model. Closed arguments are bounded to 1 MiB; all history
and schemas count against the context budget without silent truncation. Every
tool event records arguments and result. Modified files and command checks come
from the runtime ledger; sanitized model prose is displayed separately and
cannot turn an unrun check into a pass. Tool output is untrusted data. Denial is
not permission to retry an equivalent action through a different tool.

The motivating gaps are concrete: a new nested module lacks directory creation;
renaming a module and references requires several coordinated changes; obsolete
tests cannot be deleted; ordinary builds write outputs and launch compilers.
Even `git status` needs metadata reads blocked by the current sandbox. Adding
argv entries cannot overcome these restrictions, and editing source to bypass
them is prohibited. Current default command limits are 30 seconds and 16,384
combined output bytes; both are configurable within existing bounds.

## General patch contract

### Format choice

| Candidate | Benefits | Costs and decision |
| --- | --- | --- |
| Existing unique-span patch plus `create_file` | Small, proven schemas; bounded targeted edits and simple local-model calls. | Cannot express delete/move/parents or one reviewed multi-file intent. Retain as the compatibility baseline. |
| Unified diff string | Familiar, compact for many line edits; useful human preview. | Hunk counts, headers, newline conventions and rename extensions add parser ambiguity; fuzzy application risks unintended edits. Would still need trusted hash/path/approval metadata. Do not accept it as the model mutation API. |
| Structured operation list with exact-span edits | Explicit add/update/delete/move, hashes and closed fields; reuse current matching and evidence behavior. | More JSON/escaping and nested records; repeated old text costs context. Select this bounded contract, subject to model trials before default exposure. |

Use JSON arguments to `apply_patch`, contract version 2. This is a proposed API,
not valid input to today's tool. The envelope has exactly `version` (integer 2)
and `changes` (a nonempty ordered array). Each record has exactly the fields of
one of these four shapes; `sha256` below denotes the 64 lowercase hex characters
returned by `read_file`, not a hash the model must compute:

```json
{
  "version": 2,
  "changes": [
    {"op":"add","path":"src/widget/mod.rs","content":"pub fn value() -> u32 { 1 }\n"},
    {"op":"update","path":"src/lib.rs","sha256":"<read_file sha256>","edits":[{"old_text":"mod old;","new_text":"mod widget;"}]},
    {"op":"delete","path":"tests/obsolete.rs","sha256":"<read_file sha256>"},
    {"op":"move","path":"src/old.rs","to":"src/widget/value.rs","sha256":"<read_file sha256>"}
  ]
}
```

The example uses hash placeholders for readability; they fail runtime validation.
`add` takes complete content, including empty text. `update` requires one or more
edits. `delete` removes one regular file. `move` transfers one source to an absent
destination with unchanged bytes and hash. It has exactly `op`, `path`, `to` and
`sha256`; reject `edits` even when empty. Edits belong to updates only.
No directory deletion/move, overwrite, binary, chmod, ownership change or arbitrary
metadata API is included. Empty directories are not a standalone patch operation; commands may eventually create them.

Advertise these four closed record shapes and the closed edit shape
`{old_text: string, new_text: string}`. Use a discriminator plus `oneOf` for the
record schema when the provider supports it; validate it identically in Rust
regardless of provider schema enforcement. The provider compatibility milestone
must test nested schema delivery. If Ollama/model support is inadequate, keep
legacy mode rather than silently accepting extra fields or changing the contract.

### Matching, paths and bounds

- Every existing source must have been read in this session at the supplied hash.
  Validate its current bytes and identity before preview, after approval and
  immediately before each operation. A read hash authorizes no action by itself.
  A changed inode, even with equal contents, invalidates an approved preview.
- Each edit's `old_text` is nonempty, occurs exactly once in the original source,
  contains no NUL and differs from `new_text`. All matches use original bytes;
  matched ranges must not overlap. Apply replacements simultaneously in source
  order, without fuzzy matching, line-number offsets or newline normalization.
  Insertions use an existing unique anchor; empty files can be added or deleted,
  but updating an empty file is outside this initial contract.
- Paths are UTF-8 project-relative `/`-separated names, at most 4,096 bytes.
  Reject empty paths, absolute paths, empty/`.`/`..` components, NUL, trailing
  slashes and case-insensitive `.git` components. No glob/tilde/env expansion.
  This stricter v2 spelling avoids aliases; legacy normalization stays unchanged.
  Root identity and descriptor-relative no-follow traversal remain mandatory.
- Existing sources must be regular single-hard-link files; reject symlinks even
  to in-root data, devices, FIFOs, sockets and directory targets. Destination
  absence is checked without following symlinks and enforced exclusively at
  publication. Detect collisions using actual filesystem name/identity behavior,
  including case-insensitive volumes, not string comparison alone.
- A path may participate in only one change, as source or destination. Reject
  duplicate/aliased paths, move cycles, swaps, case-only moves, and file/directory
  prefix conflicts. No operation may depend on another's output. Rename and
  update references uses one unchanged move plus updates to other files. A moved
  source/destination cannot also be updated in that patch. Supporting chained file
  operations would complicate stale-state semantics; sharing newly created parent
  directories is allowed as below.
- Missing parent directories of adds/moves are created automatically as part of
  the approved patch. List them explicitly in the preview. Existing parents must
  be directories without symlinks; record/recheck their identities. Create new
  directories exclusively, mode 0700 subject to umask; a parent that appears
  during approval causes stale-state failure rather than implicit adoption.
  Deduplicate shared missing parents in the plan and create them once, in
  ancestor-first order when first needed. Later operations accept only the
  recorded identity of that plan-owned directory, not an externally substituted
  directory. Associate its effect with the first operation that created it.
- Initial limits: eight changes, sixteen edits per update source, 64 KiB per source
  or resulting file, 256 KiB total resulting file content (including unchanged move
  destinations), and 1 MiB serialized arguments, with at most 32 components per
  path and 32 new directories per plan.
  Contents are UTF-8 without NULs. These are safety ceilings, not a
  promise the model/context budget can produce that much. Reject over-limit,
  unknown-version, missing/extra-field, wrong-type, invalid-hash and malformed
  input before approval or mutation; never truncate, repair or apply a prefix.

### Approval, publication and partial success

Perform complete syntax/policy validation and preflight all changes first, with
no target mutations or directory creation. Produce one immutable plan containing
root/parent/source identities, hashes, source metadata observations and execution
constraints, absent destinations/parents and ordered operations. One per-call
approval covers exactly this plan and its digest.
Show every source/destination, operation type, byte counts, directory creation,
metadata constraints and bounded escaped diffs, including full-file deletion.
An unchanged move lists both paths, unchanged-content hash and byte count without
a content diff. Never omit a path from the summary. Use an 8,192 escaped-character
content preview budget with explicit truncation; provide a CLI option to inspect the complete
escaped plan before submitting approval. EOF, blank, No and cancellation deny
publication. Editing or refreshing the plan requires a new preview and approval.

The batch is **preflighted together but committed sequentially**. There is no
claim of cross-file atomicity or crash-safe rollback. Revalidate the whole plan
after approval, then each operation's root, parents, source/hash and destination
immediately before its first mutation. Any mismatch before mutation returns
`failed`, reason `stale_state`, `executed: false`; no automatic rebase. A mismatch
or failure after earlier changes stops remaining operations and reports partial
success.

Use descriptor-relative, owned same-directory staging files with synced content
for adds/updates and move destinations. Updates use atomic replacement; adds use
exclusive no-clobber publication as in current creation. Deletion unlinks only
the revalidated source entry. A move is explicitly destination publication then
source unlink, not a filesystem-wide atomic rename: stage the final content,
publish exclusively, record destination evidence, recheck the source and unlink
it. If unlink fails, both files remain and the result reports the duplicate and
incomplete move. This avoids overwriting a destination racing with approval.
Readers may observe intermediate batch states; tools remain sequential but
external readers are not locked out.

Preserve ordinary permission bits on updates/moves and use 0600 subject to umask
for adds. Read-only planning records source special mode bits, flags, ACLs and
xattrs, distinguishing observed presence/absence from inspection failure or
unsupported detection. Ordinary OS provenance attributes must not block preview
planning. Changed observations are stale state. Metadata inspection that cannot
establish safety becomes an explicit execution constraint, never permission to
mutate. Before any target write, the future executor must independently establish
metadata support, safely preserve attributes or refuse execution, and revalidate
after consent and before each mutation. Existing v1 metadata behavior remains
unchanged. Missing-parent creation is also a recorded substep: if subsequent file
publication fails, empty directories may remain and must be reported. Do not automatically remove them
or reverse successful files; cleanup removes only identity-checked owned staging
artifacts. Undo is a new reviewed patch against current hashes, never Git reset.

After any visible mutation, update in-memory evidence ownership before the next
fallible/cancellation point. Cancellation stops new substeps, cleans owned
staging and reports committed substeps, including a move whose source remains.
Invalidate read hashes for touched sources/destinations; the next patch needs a
fresh read. On process/host crash there is no durable session recovery guarantee;
partial files/directories may remain. Do not retry a lost-response patch blindly:
reinspect actual state and obtain new approval. Durable journaling would be a
separate design if later required.

Return one result per input change, in order: operation/path/destination,
`status`, `executed`, committed substeps, before/after hashes (absent as null),
created directories and bounded error details. Top-level statuses are `ok`,
`failed`, `denied`, `canceled`, `limit_reached` or `partial`; use a machine-readable
reason such as `stale_state`, `invalid_arguments` or `io_error`. `executed` is true
if any visible mutation committed, including directory creation. `partial` means
some but not all planned substeps committed, with a separate stop reason for
cancellation/limits. Untouched remaining changes are `not_run`. Preflight errors
have zero committed substeps; failure after effects must never say nothing ran.

## Command capability and authorization

### Model contract and permission classes

The proposed closed `exec_command` arguments are (all four fields are required):

```json
{"argv":["/usr/bin/some-program","argument"],"cwd":".","permission":"read_only","timeout_ms":30000}
```

`argv` is 1–64 nonempty NUL-free strings, with an existing absolute executable;
`cwd` is `.` or a validated existing project-relative directory. `permission`
selects a requested class from the table, never a grant. `timeout_ms` is a positive
integer bounded by the session ceiling (default 30,000; maximum 86,400,000).
Arguments remain capped at 1 MiB; output defaults to 16,384 combined bytes under
the host setting. No model-supplied environment, stdin, network switch, arbitrary
write roots or shell command string. Literal metacharacters are argv data.

| Permission class | Read/write boundary and purpose | Authorization and availability |
| --- | --- | --- |
| `read_only` | Project/runtime reads, private scratch writes; no Git metadata or network. Search and compatible verification. | Exact argv startup policy and per-action approval. Can wrap today's backend; descendants remain denied. |
| `build` | Read-only source snapshot; writes only to a fresh private output/cache tree, HOME and TMP. Supports validated compiler/test descendants. | New isolated backend and explicit session opt-in, approved executable/argument/output profile and per-action approval. Not available today. |
| `project_write` | Writes only to an isolated working copy of the project, then proposes a diff for import through the general patch tool. No live-root writes by the process. | Requires validated build isolation, explicit write opt-in and two approvals: command execution, then actual resulting patch import. Not available today. |
| `git_read` | Trusted host-generated read-only metadata snapshot plus project reads; no access to live Git administration paths. | Separate opt-in and exact approved Git inspection argv/profile; validated snapshot support required. Not available today. |

`mkdir`, formatters and generators eventually use `project_write`; the result
becomes a reviewed patch (including its required parents). Builds must redirect
outputs/caches to the approved private tree, e.g. a host-set `CARGO_TARGET_DIR`.
A tool that still writes beside source fails or uses `project_write` isolation;
do not widen the read-only source mount. An empty-directory-only command can
operate in the isolated copy but importing empty directories is deferred; file
additions already carry their parents. This deliberate limit should be reported.

Initially keep exact executable-plus-argv approvals. Later reviewed profiles may
admit specific argument patterns (e.g. bounded search paths or build targets),
but must reject response files, configuration/plugin loading, output/path escapes,
Git `-C`/`--git-dir`/config overrides and interpreter options outside that profile.
Use fixed canonical executable identity and revalidate at launch; PATH lookup
must not select project programs. Project code executed by an already-allowlisted
read-only interpreter remains constrained by today's sandbox. New write-capable
script/build profiles require the isolated backend, with snapshot hashes and
permitted descendants bound to approval. Exact argv alone does not make an
interpreter, shell or build script read-only. No generic shell profile is enabled
by this decision; a future `/bin/sh -c` mode requires a separate profile and the
same write/process/network validation gates, not a looser alternate entry point.

### Sandbox, project, Git and network boundaries

Use a private ordinary-file project snapshot and private outputs for the new
backend. Processes never write the live root. Copy without following symlinks;
reject multiply-linked files before reading their data, rather than copying a
potential outside/Git alias into the snapshot. Reject other unsupported entries
and exclude Git administration paths. Bound snapshot scans by entry/byte quotas,
cancellation and deadline; record source hashes/identities. Snapshot inconsistency
fails before launch. Source
mounts/copies for `build` must actually be OS-enforced read-only to the child;
filesystem modes alone are insufficient for a same-user process.

After `project_write`, scan/hash the isolated tree, including untracked and ignored
files, and produce an explicit diff against the input snapshot. Track adds,
updates, deletions and inferred moves (a move can conservatively be delete+add).
Do not automatically import symlinks, hard links, binaries, special files,
metadata changes or patches above v2 limits. Report unsupported results and
bounded artifact locations for inspection; never silently drop them and claim
all changes imported. The host prepares the import proposal by reading affected
live files through the same bounded file layer, recording read evidence and
requiring those hashes to match the input snapshot. It must not replace a stale
baseline with freshly changed content. Display the exact resulting patch under
a new approval, with the same post-approval checks as model-requested patches.
User edits since snapshot creation must survive. No automatic import after nonzero exit,
cancellation or limit; preserve bounded evidence/artifacts for explicit review.

File tools and command import deny `.git` components and resolved administration
directories, including linked worktree common dirs, gitdir pointer files and
nested repository metadata. The child must not discover an enclosing live
repository through ancestor paths or inherited Git variables. For `git_read`, a
trusted host routine resolves the selected worktree and common Git dirs, copies
only required metadata into a read-only private snapshot without following unsafe
links, and records its revision. Scope commands initially to status, diff and log
inspection; disable optional locks, hooks, external diff/textconv, pagers,
credential helpers and system/global/repository config execution. Validate
worktrees, submodules and config includes before support; otherwise refuse them.
Git writes, commits, checkout/reset, remote operations and live metadata exposure
are not authorized. An unsupported Git query returns `not_run`, not fabricated
inspection output. Git data and paths in results remain untrusted.

Network is denied for all four classes, including loopback, DNS, child processes
and socket/service routes. Local Ollama access belongs to the trusted parent,
never the command child. Dependencies are provisioned separately by the user;
offline build failures are visible. Future network access requires a separate
destination-scoped policy, backend enforcement and approval design; no approval
prompt here can turn it on. Outside private data remains unreadable; installed
runtime dependencies remain an explicitly trusted read surface.

Rebuild the environment as today: fixed runtime PATH and locale, private
HOME/TMP, no API keys, proxies, SSH agents, loader/language injection variables,
parent Git variables or user config. Add only host-owned profile variables such
as output/cache locations, and show them in approval. Close inherited descriptors
other than redirected standard streams; never pass root/approval handles into
the child. No interactive TTY or inherited stdin.

### Approval, limits, failures and platform gates

Before the default-No terminal approval, show the exact escaped argv, resolved
executable identity, cwd, requested/effective class, snapshot identity, readable
scope, writable outputs, environment additions, network denial and time/output
ceilings. `project_write` explicitly says that execution produces a separate
import proposal. A command cannot provide an exact predicted diff; its preview
must describe the writable scope honestly. Bind the approval to this plan and
policy revision; changed argv/cwd/code/profile/scope needs another approval.
Startup opt-in only establishes a ceiling. It never replaces per-action consent.

Share one deadline across inspection, snapshot preparation and process execution.
Initial new-backend ceilings are 100,000 snapshot entries, 1 GiB snapshot data,
1 GiB combined writable outputs/artifacts and 64 simultaneous descendant
processes. Session configuration may lower these; raising them needs explicit
host configuration and a new preview. Quotas must be enforced during execution,
not only measured afterward. Refuse launch if required disk/process containment
is unavailable. Enforce CPU/memory
and file-size limits where the validated backend supports them; advertise actual
limits and refuse classes whose required containment cannot be enforced. Timeout,
output overflow and Ctrl-C stop execution, kill and reap the entire descendant
tree and prevent subsequent/import actions. Keep the deadline active even if
stdout/stderr close early. Descendants that change process groups/sessions or
daemonize must not survive: process-group killing alone is insufficient once
forks are permitted. Bounded captured output and cleanup errors stay in evidence.

Missing isolation, changed roots/executables, insufficient policy, scan errors,
unsupported platforms or sandbox launch failure return `not_run` with
`executed: false`. No unsandboxed retry, class escalation or equivalent retry
after denial. A launched process has `executed: true` even on failure, timeout,
cancellation or cleanup error; report exit/signal and actual changed artifacts.
A successful exit records only that command's pass, not task correctness. Import
failure is separate from command success; it may have explicit partial effects
under the patch contract. Cleanup failure stops the session before further
commands and identifies retained artifacts/processes without claiming isolation
or complete cleanup.

The current macOS Seatbelt profile cannot safely enable ordinary build/test
process trees, Git inspection or project writes merely by adding argv entries.
It denies descendants and Git reads, and allows writes only to scratch. Removing
these denies, granting the project tree, or trusting a prompt would invalidate
the tested boundary. `sandbox-exec` is deprecated and is not assumed to offer a
durable containment mechanism for escaped descendants or sufficient resource
quotas. A backend feasibility milestone may select a stronger macOS facility or
retain `not_run` if it cannot satisfy the requirements. No write-capable class
ships until real-host adversarial tests pass for private/outside reads and
writes, symlink/hard-link aliases, root/parent swaps, Git/worktree boundaries,
FD/environment leaks, network/services, output/disk/time limits and escaping
descendant cleanup. Linux/Windows remain unsupported for commands until their
own separately scoped backend and real-host tests exist; no Linux implementation
is required by this decision.

## Shared policy and evidence

Evolve the existing runtime rather than introducing a second agent runtime or a
general plugin framework. Extract only repeated action preparation/approval and
evidence ownership; retain safe operation-specific file and command backends.
Use the same pipeline for legacy wrappers and new contracts. Bounded read/list
tools keep their current no-prompt policy; mutations and commands require consent:

1. Validate the closed tool-call envelope, IDs, fields, byte/count bounds and
   session cancellation before planning. Preserve sequential calls and bounded
   recovery; do not turn malformed batches into partially executed batches.
2. Resolve capability and effective policy against the anchored root; deny
   before prompting if the backend cannot enforce the requested action.
3. Prepare an immutable action plan/preview, bind consent to its digest, and
   revalidate the relevant state after approval. Denial is a recorded result;
   another tool is not a route around it.
4. Execute bounded substeps, owning launch/publication evidence immediately.
   Cancellation prevents new work but cannot erase already committed work.
5. Append structured results and render separate model prose/execution evidence.

Extend the existing event shape with tool call/action ID, contract version,
policy revision, plan digest, approval outcome, effective permission/scope,
backend identity, timing/limits and reason codes. Each file effect records
normalized source/destination, before/after hash and existence, bytes, created
directories and committed substeps. Track deleted paths and both sides of moves
in the modified-file report, even though a deleted path has no current hash.
Commands record exact argv/cwd, snapshot/output scope, launch flag, exit/signal,
bounded output, limit/cancel/cleanup reason, and discovered effects; imported
effects link to their separately approved patch action. No process launch means
no exit code and no verification pass. A command in a snapshot verifies that
snapshot; it does not prove a subsequently edited live tree passed. Display the
verified snapshot/patch identity and mark changed-source verification stale.

Use distinct `denied`, `not_run`, `failed`, `canceled`, `limit_reached`, `partial`
and success outcomes, plus structured reasons and per-substep execution flags.
Preserve failed checks when later checks pass. Unknown/undiscoverable effects
must be marked unknown, never reported as an empty diff. On new-backend mutation
tracking failure, do not import and stop for inspection. Private output writes
are artifacts, not live modified files. Temporary internal staging cleanup is
distinct from approved user-visible effects.

Keep full validated patch intent in the in-memory ledger as today; bound and
escape terminal/model output, including paths, control characters and Unicode
formatting characters. Do not persist source content or secrets by default.
If an explicit diagnostic export is later added, require bounded redacted data
and user-selected destination. Existing sessions are in-memory, not a promised
durable audit log. File contents, command output, Git text and model text remain
untrusted data and cannot rewrite policy or approve actions. Model claims never
replace ledger verification, including after errors or partial results.

## Local-model reliability and evaluation

Fewer tool names reduce repeated descriptions, tool-choice ambiguity and routing
cost, especially when all schemas/history are resent. Narrow tools offer simpler
schemas and easier individual recovery; a nested generic patch can increase
syntax errors, hash/path confusion and accidental deletion intent. Structured
span edits reuse learned v1 behavior but repeat source text and require JSON
escaping. Full-file adds consume output/context tokens; unified diffs save some
text but introduce hunk grammar. Multi-file previews/results also grow history.
Do not raise generation/context limits, silently compact history, or weaken
validation merely to fit a large patch; split independent approved changes when
needed. Malformed input must fail safely regardless of model ability.

Evaluate both local `qwen3.8:27b` and `qwen3-coder:30b` without downloading or
switching tags automatically. Prior trials in the
[validation record](validation.md#actual-model-trials) concern the old contract;
they establish no v2 success rate. Preserve their fixture as the legacy baseline.

Deterministic follow-up contract tests must cover each operation and mixed
batches; multi-edit original-byte matching; malformed/unknown/oversized JSON;
missing/stale hashes; ambiguous/overlapping spans; missing parents; path/case
aliases, collisions and cycles; metadata refusals; destination/source races;
denial/EOF; cancellation and injected I/O failure at every visible substep;
partial move/parent creation; evidence ownership and legacy behavior. Scripted
provider tests verify closed schemas, tool result IDs/order, unsupported schema
handling, error feedback and model claims contradicted by the ledger. Command
policy tests cover class requests exceeding grants and all platform failure
paths; real-host tests, not mocks, establish isolation and descendant cleanup.

Extend the existing fixture evaluator with disposable projects and fixed prompts
for: inspect/search and tool choice; the original fix/check recovery; add a
nested module; move a module and update references; delete an obsolete test;
mixed multi-file changes; recovery after malformed/stale patch feedback; and
refusal after denied commands or injected tool-output instructions. Exercise
actual command capabilities only after their backend gates pass. Fixture-only
approvals must record decisions and never become normal automatic approvals.

For each tag and legacy/v2 mode, run the same prompts at least three times on
the same source revision and snapshot. Record model digest/quantization, Ollama
version, hardware, provider/schema/prompt hashes, context/generation settings,
attempts, raw bounded calls/results, schema errors, recovery steps, unintended
effects, tool choices, token counts where supplied (serialized bytes otherwise),
elapsed time and ledger-grounded task success/failure. Report unsupported/missing
models and incomplete trials as Not run/failed with reasons. Do not download,
redeem usage resets, buy allowance or switch providers to complete trials.

Before retiring legacy interfaces, require deterministic parity, no unapproved
effects, all three filesystem workflows and the legacy edit/check flow completed
at least once per target tag, and a published comparison of failures, retries and
context/time costs. Zero
observed escapes is a necessary gate, not proof of safety. Maintainers must
assess observed regressions before changing the default; no success percentage
is invented by this design. A poorly supported schema keeps v2 opt-in and legacy
available while the contract is revised and reevaluated. The build-output
workflow has the same per-tag completion gate before making `build` a default
capability; it does not block patch migration while that backend remains absent.

## Ordered implementation roadmap

These are follow-up issue/PR-sized milestones, not implementation performed by
Issue #16. Keep one runtime and policy source; each milestone states its evidence
and rollback boundary. Do not build two competing execution frameworks.

| Order | Deliverable and prerequisites | Measurable acceptance and compatibility |
| --- | --- | --- |
| 1 | Extract shared action-plan/approval/evidence helpers from current mutation/command paths. | Existing deterministic and real macOS security tests retain current schemas, previews, statuses and cancellation behavior; add only missing ownership assertions. Default behavior unchanged; revert extraction independently. |
| 2 | Implement pure v2 parser, path/conflict validator and preview planner, recording metadata observations and execution constraints. Depends on 1. | Golden add/update/delete/unchanged-move plans, strict bounds and malformed inputs, native source planning with ordinary OS attributes, real filesystem alias tests; zero target mutations during planning. No model exposure or execution; revert without changing v1. |
| 3 | Implement descriptor-relative v2 executor behind an explicit experimental session mode. Depends on 2. | Failure/cancel/race injection proves per-substep effects, no-clobber publication, stale checks, parent creation and partial-result ledger; preview-before-consent CLI tests pass. No command-policy changes; v1 remains default. |
| 4 | Add session-selected schema/provider support and real-model v1/v2 fixture comparison. Depends on 3. | Test both tags/schema transport and report the evaluation matrix above. Advertise exactly one `apply_patch` signature per session; v1 maps to shared primitives without imposing stricter v2 path/metadata rules, and `create_file` retains no-parent/no-overwrite semantics. Unsupported models keep v1. Never reinterpret an old in-flight call as v2. |
| 5 | Introduce `exec_command` read-only contract as a wrapper around the existing exact-argv backend. Depends on 1 and 4. | Policy/unit and actual CLI/macOS tests show no extra permissions, cwd confinement, ceilings and evidence; `run_command` stays a compatible wrapper. Schema mode is fixed for a session, no duplicate command tool names advertised. Roll back to old advertisement without weakening isolation. |
| 6 | Prototype isolated build backend, initially host/test-only. Depends on 5. | Document chosen macOS containment and prove real-host read-only source, output quotas, network/FD/environment boundaries and descendant-tree cancellation (including setsid/double-fork). If containment is unavailable, retain `not_run`; no write flags exposed. |
| 7 | Expose opt-in `build` only after 6 passes. | Offline representative build/test writes solely to private outputs and kills all descendants on time/output/cancel limits; snapshot identity appears in checks. Existing read-only regressions pass; disabling the class restores the previous ceiling. |
| 8 | Add opt-in isolated `project_write` and explicit patch import. Depends on 3, 4 and 7. | Formatter/generator/mkdir fixtures produce reviewed diffs; second approval, stale live source, unsupported effects, import failure/partial success and user-edit preservation are tested. No process writes live source. Disable the class without deleting approved edits/artifacts. |
| 9 | Add `git_read` snapshot support, after 7's containment evidence. | Real normal/linked-worktree fixtures verify status/diff/log without live metadata access, config execution, locks, network or Git writes; unsupported layouts fail closed. Opt-in only; disabling it restores Git refusal. |
| 10 | Consider default v2 advertisement and retirement of redundant model interfaces. Depends on 4, 5 and published parity evidence; 7–9 are prerequisites only for advertising those classes. | Meet the local-model retirement gates, document schema selection/deprecation, preserve a release rollback to legacy mode and rerun affected deterministic/macOS checks. Retain `read_file`/`list_files` unless replacement proves their hashing/bounded-read benefits. No shell/write permission is gained by retiring a tool name. |

Every new capability needs implementation-guide/current README updates and a
fresh validation record. Disabling an experimental mode affects future actions,
never rolls back user files. Do not support changing schema versions mid-session:
retained tool history must stay interpretable. A future write-capable shell mode
requires milestones 6–8, adversarial child/resource/network validation, a specific
shell/executable profile and explicit session/per-action authorization; it needs
its own follow-up decision before exposure.

## Acceptance and validation record

| Issue criterion | Decision evidence |
| --- | --- |
| AC1 | Current implementation table, source links, loop/sandbox/approval/evidence analysis and concrete workflow gaps. |
| AC2 | Selected v2 operation shapes, strict matching/paths/bounds, parents, immutable preview, sequential commits and partial-result/recovery rules. |
| AC3 | Command contract/classes, isolated outputs/import, Git/network/environment boundaries, cancellation and real-host platform gates. |
| AC4 | Shared pipeline and event fields, committed-substep tracking, verification identity and untrusted-output treatment. |
| AC5 | Format/tool-count trade-offs, deterministic fixtures, two-tag repeated evaluation and retirement gates; no new model success claims. |
| AC6 | Ordered milestones, dependencies, measurable checks and compatibility/rollback conditions. |
| AC7 | Canonical guidance links and actual documentation check results recorded below. |

Validated on 2026-11-10 in the assigned macOS worktree:

| Check | Actual result |
| --- | --- |
| Current Issue/discussion and repository inspection | **Pass**: fetched Issue #16 and its comments (none); checked the five schemas, runtime, agent loop, command profile and canonical guidance. |
| `cargo run --locked --example check_docs` | **Pass**: 14 Markdown documents, zero relative-link, heading-anchor, fence or template-metadata errors. |
| Complete documentation review | **Pass**: reviewed the new decision and all four linked-guide diffs against AC1–AC7, including current/future distinctions, patch partial effects, command gates, compatibility and historical MVP accuracy. |
| `git diff --check` and staged equivalent | **Pass**: no whitespace errors, including the staged new architecture file. |

Future runtime/model tests above are acceptance requirements for their milestones,
not claimed results of this documentation-only change. The docs checker checks
local links/anchors; it does not validate remote link availability. The Issue URL
was accessed through the GitHub connector; no other remote references were added.

Optional current Codex source comparison and new Ollama evaluations: **Not run**;
no runtime contract was implemented. Runtime tests/build/lint: **Not run** for
this documentation-only decision, following the
[change-based validation guidance](testing-ci-principles.md#current-tooling-and-change-based-validation).
Required post-merge verification: none required.
