# Tool architecture decision (Issue #16)

Status: architecture decision from
[Issue #16](https://github.com/takahirox/cognix/issues/16), revised by
[Issue #22](https://github.com/takahirox/cognix/issues/22) to simplify initial v2
delivery. Runtime work remains in the milestones below.

Milestone 2's [internal parser and read-only planner](patch-planning.md) is now
implemented. V2 remains unavailable to models and has no executor or CLI mode;
the validation record in that guide includes native planning with source metadata
observations and execution constraints. Later milestones remain future work.

[Issue #28](https://github.com/takahirox/cognix/issues/28) introduced the current
`run_command` and `apply_patch` contracts, retiring the standalone listing,
reading, and creation tools. See the [implementation guide](implementation.md)
for the shipped interface. The five-tool baseline and proposed v2 roadmap below
record the design before that change.

[Issue #30](https://github.com/takahirox/cognix/issues/30) added independent
approval modes, live workspace writes, and destination-scoped HTTP(S) review
at the existing tool boundary.

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
enforced boundary. Full access requires a deliberate session setting.

The general editing/command pattern is motivated by the coding-agent reference
in the Issue. This is an independent Cognix contract, not a claim that Codex has
the same approval, patch, atomicity or sandbox semantics. Comparing current
Codex source is optional and was not performed for this decision.

Desktop UI, MCP, multiple agents, Linux sandbox implementation, automatic Git
commits and cloud inference are outside this work. Normal inference remains
local Ollama, without automatic model/provider fallback or downloads.

## Five-tool baseline and its limits

This analysis began with the source inherited from `ff984a9` and reflects the
historical command approval policy from Issue #26. The
[implementation guide](implementation.md) documents current behavior, and the
[MVP specification](mvp.md#f4--minimal-tools) preserves the original four-tool
scope and the later creation extension.

| Tool before Issue #28 | Capability | Constraint and evidence |
| --- | --- | --- |
| `list_files(path)` | Sorted, single-directory listing; `.` selects root. | Omits symlinks and case-insensitive `.git`; at most 500 entries and the output byte limit. No recursive search. |
| `read_file(path)` | Reads a regular, single-hard-link UTF-8 file without NULs, at most 64 KiB. | Returns content and SHA-256; records the normalized path/hash in the runtime's seen map. No binary or large-file editing. |
| `apply_patch(path, sha256, old_text, new_text)` | Replaces exactly one unique nonempty span in a previously read existing file. | Rejects stale hashes, ambiguous/no-op spans and oversized results. Diff and exact replacement precede approval; same-directory atomic replacement preserves ordinary mode bits, but not ACLs/xattrs. No add/delete/move/multiple-file operation. |
| `create_file(path, content)` | Publishes one new regular file with complete UTF-8 content, at most 64 KiB, including empty content. | Existing parents only; no overwrite. Preview/approval and descriptor-relative exclusive publication; mode 0600 subject to umask. Read it before subsequently patching. |
| `run_command(argv)` | Executes one exact argv after separate approval, subject to any configured allowlist. | Absolute installed runtime executable, no shell expansion, project read-only, scratch writes only, no network or descendants, `.git` unreadable. |

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

The baseline [command backend](../src/commands.rs) used macOS Seatbelt via
`sandbox-exec` with deny-by-default rules. Runtime/project reads and scratch writes are permitted;
outside data, network, project writes, Git data, process spawning/forking and Mach
service lookups are denied. The environment is rebuilt, inherited descriptors
above stderr are closed on exec, stdin is null, and launch failure never retries
unsandboxed. After approval a bounded project scan rejects hard-link aliases
even when absent from argv. Unsupported platforms return `not_run` before
approval/launch. The current profiles are described in the
[implementation guide](implementation.md#verification-policy-and-isolation).

[agent.rs](../src/agent.rs) executes calls sequentially in response order, with
16 model iterations by default and at most eight calls per response. Malformed
envelopes/duplicate call IDs stop a batch before effects; ordinary argument/tool
errors return to the model. Closed arguments are bounded to 1 MiB; all history
and schemas count against the context budget without silent truncation. Every
tool event records arguments and result. Modified files and command checks come
from the runtime ledger; sanitized model prose is displayed separately and
cannot turn an unrun check into a pass. Tool output is untrusted data. Denial is
not permission to retry an equivalent action through a different tool.

Baseline gaps motivating the design included missing directory creation for a new nested module;
renaming a module and references requires several coordinated changes; obsolete
tests cannot be deleted; ordinary builds wrote outputs and launched compilers outside the original profile.
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
edits. `delete` removes one regular file. `move` has exactly `op`, `path`, `to`
and `sha256`: it transfers unchanged source bytes to an absent destination.
An `edits` field on a move is invalid, even when empty. The destination hash
equals the source hash. Move a module and update references with one move plus
updates to other files in the same reviewed patch. Editing the moved file needs
a later patch after reading its destination; move-with-edit is deferred unless
a concrete need justifies extending the contract. No directory deletion/move,
overwrite, binary, chmod, ownership change or arbitrary metadata API is included.
Empty directories are not a standalone patch operation; commands may eventually
create them.

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

### Read-only planning and execution metadata

Planning inspects the anchored filesystem without target mutations. Keep all
path, prior-read hash, file identity, regular-file/single-hard-link, no-symlink,
parent identity and destination-absence checks. A source extended attribute
alone, including ordinary macOS `com.apple.provenance`, must not block a preview.
Record relevant mode bits, file flags, ACL/xattr observations and execution
constraints in the immutable plan and escaped preview. Metadata presence is
distinct from an unsupported or failed inspection; record that distinction. If
metadata safety cannot yet be established, mark execution eligibility as
unestablished rather than treating missing evidence as permission. Failure to establish safe path or
file identity still fails planning.

A plan is neither consent nor proof that the executor can publish safely. Before
any target write, the future executor must check the whole plan against its
operation-specific metadata support, then revalidate relevant metadata with
identity/hash checks after approval and immediately before each mutation. Changed
metadata invalidates the plan. Updates/moves must preserve source attributes
safely; never strip attributes or silently replace the source with a metadata-free
file. Unsupported metadata at the initial whole-plan gate returns an explicit
unsupported-metadata result with no target writes. A metadata race or newly
unsupported state after earlier effects stops the batch with accurate partial
results under the rules below.
Deletion must validate its source metadata policy too, although it has no
replacement metadata to copy. Initial execution may refuse ACLs/xattrs or special
mode bits that it cannot safely handle; this does not prevent read-only planning.
Safe detection and any preservation behavior need real-host tests before v2
mutation ships. V1 metadata behavior is unchanged.

### Approval, publication and partial success

Perform complete syntax/policy validation and preflight all changes first, with
no target mutations or directory creation. Produce one immutable plan containing
root/parent/source identities, hashes, absent destinations/parents, source metadata
observations, execution constraints and ordered operations. A preview can exist
even when execution is unsupported; the internal planner never prompts for
approval. The executor must establish execution eligibility before requesting
consent. One per-call approval covers exactly this plan and its digest.
Show every source/destination, operation type, byte counts, directory creation,
metadata constraints and bounded escaped diffs, including full-file deletion. For moves show both
paths, the unchanged-content hash/byte count and metadata constraints, without
an edit diff.
Never omit a path from the summary. Use an 8,192 escaped-character content preview
budget with explicit truncation; provide a CLI option to inspect the complete
escaped plan before submitting approval. EOF, blank, No and cancellation deny
publication. Editing or refreshing the plan requires a new preview and approval.

The batch is **preflighted together but committed sequentially**. There is no
claim of cross-file atomicity or crash-safe rollback. Revalidate the whole plan
after approval, then each operation's root, parents, source/hash, destination and
metadata immediately before its first mutation. Any mismatch before any batch
mutation returns `failed`, reason `stale_state`, `executed: false`; no automatic
rebase. A mismatch or failure after earlier changes stops remaining operations
and reports partial success.

Use descriptor-relative, owned same-directory staging files with synced content
for adds/updates and move destinations. Updates use atomic replacement; adds use
exclusive no-clobber publication as in current creation. Deletion unlinks only
the revalidated source entry. A move is explicitly destination publication then
source unlink, not a filesystem-wide atomic rename: stage the unchanged source
bytes with safely preserved metadata, publish exclusively, record destination
evidence, recheck the source and unlink it. If unlink fails, both files remain
and the result reports the duplicate and incomplete move. This avoids overwriting
a destination racing with approval.
Readers may observe intermediate batch states; tools remain sequential but
external readers are not locked out.

Preserve ordinary permission bits on updates/moves and use 0600 subject to umask
for adds. Apply the execution metadata gate above before creating even a missing
parent; approval cannot override unsupported metadata handling. Missing-parent
creation is also a recorded substep: if subsequent file publication fails, empty
directories may remain and must be reported. Do not automatically remove them
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

### Shipped command and approval contract

`run_command` continues to accept literal argv with no per-call sandbox
switch. The host selects `read-only` or `workspace-write` independently of
whether per-action approval is manual or automatic. Workspace-write uses the
live project tree for build output, formatters and generators. Changes are
immediately visible in the project. Command limitations and optional exact-argv
restrictions apply before human or LLM review. Full access is an explicit
session-wide choice.

| Profile | Command permissions |
| --- | --- |
| `read-only` | Runtime and project reads, scratch writes, and no child processes. |
| `workspace-write` | Project and runtime reads, including macOS system Ruby gems, with live project writes and subprocess cleanup scoped to a dedicated macOS resource coalition. |
| Full access approval mode | Commands use ordinary OS access and the inherited environment without a Cognix sandbox or approval. |

In the sandboxed profiles, the selected command starts from the project root,
uses a rebuilt environment with private HOME/TMP, receives null stdin, and
inherits no application descriptors. Seatbelt protects outside file data, Git
metadata, network and service endpoints. An exact argv entry does not turn
project code or an interpreter into a safe inspection. Only fixed no-argument
system inspection commands skip approval. Changes outside sandbox policy fail
at execution even when a reviewer approves the command. Command results
preserve exit/signal and bounded output. A project command can write directly
without generating a patch-ledger entry.

Workspace-write uses a temporary launchd job with a dedicated resource coalition.
Cognix verifies the coalition before the sandboxed program starts. Children
created by `posix_spawn` inherit both Seatbelt and coalition membership. Cleanup
signals coalition members by process identity and checks the kernel task count
until it reaches zero. Full-access commands use process-group cleanup and can
detach. Cleanup provides no resource quotas.

### Destination-scoped network requests

Sandboxed project processes keep direct network blocked, including loopback. A literal
`curl` or `/usr/bin/curl` plus one HTTP(S) URL asks the host for a fixed GET
transport. The hosted action uses the exact validated URL, the system HTTP
client and private scratch. The scope shows the protocol, host, port, URL and
method. A sandboxed command with `http_get: true` can submit an exact URL over a
private per-command Unix socket. Cognix includes the parent command and the
fresh network scope in review evidence, then uses the same system transport
for the approved request. The transport receives no project headers, bodies,
flags, proxy settings or redirect permissions. A new destination or a
subsequent request gets its own policy and approval check. Other transports
remain unavailable to sandboxed commands.

Manual mode asks the user to approve the exact request. Auto mode can approve
an unlisted destination when justified by user intent. `deny_network` entries
apply before review. No prior request approval grants rights to a later one.
There is no escalation based solely on the failure of a sandboxed subprocess.
Cognix's existing loopback inference transport is separate from command network
permissions.

### Future Git inspection

A future `git_read` profile may use a trusted host-generated, read-only metadata
snapshot. It would need validation for worktrees, submodules and config includes,
and must prevent live Git writes, hooks, external diff/textconv, credential
helpers and remote operations. Unsupported queries must remain unexecuted.
This proposal is independent of workspace-write and the internal v2 planner.

## Shared policy and evidence

Evolve the existing runtime rather than introducing a second agent runtime or a
general plugin framework. Extract only repeated action preparation/approval and
evidence ownership; retain safe operation-specific file and command backends.
Use the same pipeline for legacy wrappers and new contracts. Bounded read/list
tools can use their bounded inspection exemption. Other actions in manual or
auto mode use the same approval boundary:

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
Commands record submitted argv, launch flag, exit/signal, bounded output and
limit/cancel evidence. The action plan identifies the working directory and
effective permissions. A command without process launch has no exit code or
verification pass.

Use distinct `denied`, `not_run`, `failed`, `canceled`, `limit_reached`, `partial`
and success outcomes, plus structured reasons and per-substep execution flags.
Preserve failed checks when later checks pass. Unknown/undiscoverable effects
must be marked unknown, never reported as an empty diff. Temporary internal staging cleanup is distinct from approved user-visible
effects.

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
aliases, collisions and cycles; unchanged move bytes/hashes and rejection of
move `edits`; successful planning with ordinary OS attributes and recorded
metadata constraints; executor metadata refusals/preservation; destination/source
races; denial/EOF; cancellation and injected I/O failure at every visible substep;
partial move/parent creation; evidence ownership and legacy behavior. Scripted
provider tests verify closed schemas, tool result IDs/order, unsupported schema
handling, error feedback and model claims contradicted by the ledger. Command
policy tests cover class requests exceeding grants and all platform failure
paths; real-host tests, not mocks, establish isolation and descendant cleanup.
Executor/command checks belong to their milestones, not the internal planner.

Start format evaluation early with a small schema-only trial, alongside parser
and planner work and before broad model exposure. Use two disposable fixtures
and fixed prompts: add one file under a missing module directory, and edit three
existing files in one patch. Make one initial attempt per task with each installed
target tag, allowing at most one feedback/retry for malformed intent. Deliver
the proposed closed schema through the real provider; parse and plan returned
calls without executing them. Inspect intended paths/content, validity, schema
transport, escaping, token/serialized size and recovery. Record the model/provider
and prompt/schema revisions, settings, calls/results, limitations and observations.
Use these few samples to adjust field names, nesting or guidance before freezing
the schema for broad rollout; they establish no success rate. Missing models
are reported, never downloaded automatically. This trial is a separate follow-up,
not an acceptance criterion or model-exposure requirement for the internal
parser/planner in Issue #20.

For the fuller comparison before default v2 exposure or legacy patch retirement,
extend the existing fixture evaluator with disposable projects and fixed prompts
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

Before making v2 the default or retiring legacy patch interfaces, require
deterministic parity, no unapproved effects, all three filesystem workflows and
the legacy edit/check flow completed at least once per target tag, and a published
comparison of failures, retries and context/time costs. Zero observed escapes
is a necessary gate, not proof of safety. Maintainers must
assess observed regressions before changing the default; no success percentage
is invented by this design. A poorly supported schema keeps v2 opt-in and legacy
available while the contract is revised and reevaluated. The workspace-write command backend has its own validation record and does
not depend on patch migration.

## Ordered implementation roadmap

These are follow-up issue/PR-sized milestones, not runtime implementation
performed by Issues #16 or #22. Numbers identify milestones, not a global serial
queue: only the stated prerequisites order work. Patch and command tracks can
advance independently. Keep one runtime and policy source; each milestone states
its evidence and rollback boundary. Do not build competing execution frameworks.

| Order | Deliverable and prerequisites | Measurable acceptance and compatibility |
| --- | --- | --- |
| 1 | Extract shared action-plan/approval/evidence helpers from current mutation/command paths. | Existing deterministic and real macOS security tests retain current schemas, previews, statuses and cancellation behavior; add only missing ownership assertions. Default behavior unchanged; revert extraction independently. |
| 2 | Implement pure v2 parser, path/conflict validator and read-only preview planner, recording metadata observations and execution constraints. Depends on 1. | Golden add/update/delete/unchanged-move plans, strict bounds and malformed inputs, real filesystem alias tests, native planning with ordinary OS attributes; zero target mutations. Apply the revised Issue #20 criteria below. No model exposure, trials or execution required; revert without changing v1. |
| 2a | Run the small model-facing format trial alongside 2, using its parser/planner as available; no executor prerequisite. | Two tasks per installed target tag with bounded retry, real schema transport and read-only plan inspection as specified above. Feed findings into the schema before broad rollout. This separate trial does not gate milestone 2 acceptance. |
| 3 | Implement descriptor-relative v2 executor behind an explicit experimental session mode. Depends on 2. | Failure/cancel/race injection proves per-substep effects, unchanged moves, no-clobber publication, stale checks, parent creation and partial-result ledger; execution metadata detection/refusal or safe preservation and preview-before-consent CLI tests pass. No command-policy changes; v1 remains default. |
| 4 | Add session-selected experimental schema/provider support. Depends on 3; incorporate 2a findings before broad rollout. | Test schema transport and supported tasks on both tags. Advertise exactly one `apply_patch` signature per session; v1 maps to shared primitives without imposing stricter v2 path/metadata rules, and `create_file` retains no-parent/no-overwrite semantics. Unsupported models keep v1. Never reinterpret an old in-flight call as v2. The full comparison is reserved for 10. |
| 5 | Introduce `exec_command` read-only contract as a wrapper around the existing exact-argv backend. Depends only on 1, independently of 2–4 and patch model evaluation. | Policy/unit and actual CLI/macOS tests show no extra permissions, cwd confinement, ceilings and evidence; `run_command` stays a compatible wrapper. Schema mode is fixed for a session, no duplicate command tool names advertised. Roll back to old advertisement without weakening isolation. |
| 9 | Add opt-in `git_read` snapshot support after validating the snapshot boundary. | Real normal/linked-worktree fixtures verify status/diff/log without live metadata access, config execution, locks, network or Git writes; unsupported layouts fail closed. Disabling it restores Git refusal. |
| 10 | Consider default v2 advertisement or retirement of legacy patch interfaces. Depends on 4 and the full published legacy/v2 comparison; command wrapper delivery is independent. | Meet the local-model default/retirement gates, document schema selection/deprecation, preserve a release rollback to legacy mode and rerun affected deterministic/macOS checks. Retiring `run_command` separately needs 5 and command compatibility evidence. Retain `read_file`/`list_files` unless replacement proves their hashing/bounded-read benefits. No shell/write permission is gained by retiring a tool name. |

Every new capability needs implementation-guide/current README updates and a
fresh validation record. Disabling an experimental mode affects future actions,
never rolls back user files. Do not support changing schema versions mid-session:
retained tool history must stay interpretable. Issue #30 supersedes the former
isolated build and project-copy import milestones (6–8).

## Issue #20 and PR #21 acceptance changes

Carry the following replacements into
[Issue #20](https://github.com/takahirox/cognix/issues/20) and its existing
[PR #21](https://github.com/takahirox/cognix/pull/21). This section is the single
handoff for revising the parser/planner; it does not implement or expose v2.
Replace the move in their JSON examples with the four-field move shown above,
and update parser field rules, plan/preview goldens and developer documentation
to match. Do not accept a legacy `edits: []` on a v2 move.

Use this replacement acceptance wording (identifiers retained):

- **AC1 — Closed, typed v2 contract:** Parse only `{version: 2, changes: [...]}`
  with exactly the specified fields for `add`, `update`, `delete`, and `move`,
  and closed nested `{old_text,new_text}` edits for updates only. A move has
  exactly `op`, `path`, `to`, and `sha256`, preserves source bytes and targets an
  absent destination; reject `edits` on a move even when empty. Reject
  missing/extra fields, wrong types, unknown ops/versions, malformed SHA-256
  (exactly 64 lowercase hex chars), empty batches, bad edit shapes and oversized
  JSON without mutation. Distinguish parser errors with actionable structured
  reasons.
- **AC4 — Read-only filesystem and metadata preflight:** Use the existing
  anchored-root, descriptor-relative, no-symlink traversal and safe regular-file
  checks for planning. Validate source identity and hash against a prior
  in-session read, destination absence without following symlinks, existing
  parent identities, and creation feasibility. Reject unsupported special files
  and multiple hard links; keep path, identity and destination checks fail-closed.
  A source extended attribute alone, including ordinary OS provenance, must not
  prevent building a preview. Record relevant special mode bits, ACL/xattr
  observations, inspection support/failures and operation-specific execution
  constraints; distinguish metadata presence from inability to inspect it safely.
  Unestablished metadata safety must be an explicit execution constraint, never
  permission to mutate. Record identities, hashes, metadata state, expected
  absences and new-parent plans for milestone 3 to recheck. Planning never
  creates directories, staging files or other target/metadata mutations. A plan
  is not authorization or a promise of race-free execution: milestone 3 must
  verify metadata handling before any target write, preserve attributes safely
  or report unsupported cases, and independently revalidate after consent and
  before each mutation.
- **AC5 — Immutable, reviewable plan:** Return an ordered plan with exact
  operations, source/destination, relevant before/after bytes/hashes and existence,
  intended parent creations, byte counts, expected identities, metadata
  observations and explicit execution constraints. Generate deterministic,
  bounded and terminal-sanitized add/update/delete/move previews, including
  unchanged-content move paths/hash/byte count and every affected path/directory.
  Clearly mark truncated content and make the complete escaped plan inspectable
  in a manner consistent with this decision, without dropping any paths from the
  summary. Preparation never seeks approval or commits changes.
- **AC7 — Deterministic and real-filesystem regression coverage:** Add golden
  valid/mixed add-update-delete-unchanged-move plans and previews; reject move
  `edits`, including an empty array. Cover malformed and boundary values;
  stale/changed inode/hash; dangling symlinks and symlinked parents; hard-linked
  files; missing parents; races/identity changes between inspections; file/directory
  and case/Unicode alias conflicts; and applicable metadata observations and
  constraints, including ACL/xattr checks where supported. Native successful
  planning fixtures must work with ordinary OS attributes; do not require an
  xattr-free volume or skip mixed-source success, byte/inode races or the
  aggregate unchanged-move content boundary merely because provenance exists.
  Test metadata changes between inspections where the host supports them,
  detecting stale state rather than silently refreshing it. Successful and failed
  planning must retain AC6's before/after snapshots proving zero target mutations,
  including directories, hidden names and metadata. Executor preservation/refusal
  tests belong to milestone 3, not this internal planner. On supported macOS run
  the applicable existing runtime/agent/CLI and sandbox regressions,
  `cargo fmt --all -- --check`,
  `cargo clippy --locked --all-targets -- -D warnings`, documentation checker and
  whitespace checks. Report actual evidence and skipped checks with reasons,
  not assumed results.

AC2, AC3, AC6 and AC8 remain in force under the revised contract. In particular,
count unchanged move destinations toward the aggregate content ceiling, prohibit
updating a moved source/destination elsewhere in the same patch, preserve
no-side-effect and legacy-compatibility assertions, and document the executor's
metadata handoff. Move a file and update references in other files as a mixed
success fixture. Keep model trials outside planner acceptance; the small early
trial is milestone 2a and the full comparison gates milestone 10.

The [recorded provenance skips](https://github.com/takahirox/cognix/issues/20#issuecomment-6770667635)
explain the design revision; they are not passing evidence for the revised
planner. Revise PR #21 in place and rerun the five affected native subcases
(mixed-source success, source byte/inode/metadata inspection races and aggregate
move-content boundary) and affected regressions, recording results at its new
head. No executor, command backend, model advertisement or v1 change belongs in
that revision. This documentation change neither edits the GitHub Issue/PR nor
claims those implementation checks have run.

## Acceptance and validation record

### Original Issue #16 decision

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

### Issue #22 design revision

Reviewed on 2026-11-11 in the assigned macOS worktree against the current
Issues #16, #20 and #22, their discussions, PR #21's current description and
review state, and the canonical development/review/UI/testing guides.

| Issue #22 criterion | Design evidence | Result |
| --- | --- | --- |
| AC1 | Read-only planning records metadata constraints without rejecting ordinary xattrs; the executor checks metadata support before writes. Exact replacement #20 AC4/AC7 wording preserves native planning, zero mutations and stale-state coverage. | **Pass** (design). |
| AC2 | Four-field unchanged-content move, rejection of move edits, matching previews/tests/import rules and later move-with-edit scope. Other-file reference updates remain in the same patch. | **Pass** (design). |
| AC3 | Milestone 5 depends only on shared lifecycle work; build isolation, project-write import and Git inspection have explicit independent prerequisites and containment gates. Existing commands remain usable. | **Pass** (design). |
| AC4 | Small early two-task schema/planner trial informs rollout; full repeated legacy/v2 comparison gates default/retirement. Neither is internal planner acceptance. | **Pass** (design). |
| AC5 | Reviewed the complete four-file diff for operation/preview/metadata rules, sequential partial effects, compatibility, linked guidance and milestone dependencies; checks below. | **Pass**. |

| Check | Actual result |
| --- | --- |
| `cargo run --locked --example check_docs` | **Pass**: 14 Markdown documents, 0 errors. |
| `git diff --check` | **Pass**: no whitespace errors. |

Runtime suites, executor/command implementation and real-model trials: **Not run**
for this documentation-only revision; they remain in their respective milestones.
PR #21's five native skipped subcases still need implementation and fresh evidence
under the revised criteria; no pass is claimed for them here. Optional validation:
none required. Required post-merge verification: none required.
