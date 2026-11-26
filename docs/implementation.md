# MVP implementation

This guide describes the two tools introduced by Issue #28. The
[tool architecture decision](tool-architecture.md) records the earlier design
and future work. Its [internal v2 parser and read-only planner](patch-planning.md)
is separate from the runtime tool interface.

The supported implementation is the Rust `cognix` Cargo package. `src/agent.rs`
owns the reusable in-memory conversation and bounded loop; `provider.rs` defines
the narrow async `Provider` interface and Ollama adapter; `runtime.rs` and
`commands.rs` enforce tool and sandbox boundaries. `src/main.rs` and `terminal.rs`
provide terminal input, approval and evidence rendering. `src/lib.rs` exposes the
engine for later frontend reuse without adding a desktop framework.

Rust libraries handle HTTP (reqwest/Tokio), JSON (serde_json), hashing (sha2),
temporary scratch directories (tempfile), diffs (similar), option parsing (clap)
and Ctrl-C (ctrlc). HTTP redirects and inherited proxies are disabled. Inference
uses only loopback HTTP; there is no hosted provider or implicit model download.
Cargo.lock pins dependency versions. See the README for build/check commands.

## Shared action lifecycle

After validating the arguments and checking operation-specific preconditions,
`runtime.rs` constructs a private `ActionPlan` from `action.rs`. It holds an
immutable diff, bounded creation preview, or exact command argv. Preparing a
preview performs no writes. The terminal sanitizes the display and requires
a submitted `y` for each action. Blank input, EOF, and other answers deny it.
Cancellation takes precedence over the response.

After approval, each backend checks its boundary again before executing. File
edits and creations share staging, cleanup, and bookkeeping. The runtime records
the modified path and content hash immediately after publication, before
cleanup or cancellation reporting. Commands share the same approval lifecycle
and retain separate executable, sandbox, and process controls. Each invocation
records its arguments and result in the session ledger.

## Tools and context

All argument objects are closed: missing/extra fields and incorrect types return
structured errors. Tool calls execute sequentially in response order. Assistant
messages (including thinking and tool calls), user messages and tool results
remain in memory across interactive turns. Supplied call IDs are preserved;
missing native Ollama IDs receive session-unique IDs. Duplicate IDs or malformed
envelopes stop the batch before side effects. Tool results include both native
`tool_name` and `tool_call_id`, in original order. Invalid arguments and ordinary
tool errors return to the model for recovery. Every model request includes the
full retained history and current tool schemas. No history is silently removed.

The adapter follows Ollama's official [chat API](https://docs.ollama.com/api/chat)
and [tool-calling protocol](https://docs.ollama.com/capabilities/tool-calling).
The startup probe executes no project tool. Advertising tools alone is not enough:
the selected model must actually call the probe with the expected argument.
Model failures are actionable errors, with no automatic fallback to another tag.
The adapter rejects known cloud-backed tags/metadata. Use a trusted local server;
a server lying about its configuration is outside the client trust boundary.

| Tool | Required arguments | Behavior |
| --- | --- | --- |
| `run_command` | `argv` | Inspect, search, or verify from the project root with literal arguments, approval, and bounded sandboxed output. |
| `apply_patch` | `path`, `old_text`, `new_text` | Preview and approve a single-file update or creation. `old_text: null` selects creation. |

Updates require a unique exact `old_text` match, including overlapping
occurrences when testing uniqueness. Empty `old_text` matches only an empty
existing file. Cognix snapshots the source internally and preserves bytes
outside the matched span. The source and result must be UTF-8 without NULs,
each at most 65,536 bytes. No-op updates are rejected. An atomic same-directory
replacement preserves ordinary mode bits. Extended attributes and ACLs are
not copied. Use this patch interface on ordinary source files.

For creation, `new_text` holds the complete file, up to the same byte limit.
Empty content is allowed. The destination's parent must already exist.
File deletion, moves, multi-file patches, and directory creation are unsupported.

An update preview shows a diff and exact replacement. A creation preview
shows the destination, byte count, and additions, with up to 8,192 escaped
characters of content. Truncation and empty content are marked explicitly.
The event ledger records the full arguments.

After approval and immediately before publication, Cognix rechecks the root
and parent identities, the update's original bytes and file fingerprint, or
the creation's expected absence. Staging uses descriptor-relative
`openat(O_CREAT | O_EXCL | O_NOFOLLOW)`. Content is written and synced before
publication. Updates use `renameat` and creations use exclusive `linkat`, which
refuses a concurrently created destination. A new file has mode 0600, subject to
umask. Staging cleanup checks the name still belongs to its original inode.

Success returns `status: "ok"`, `executed: true`, normalized `path`, `sha256`,
and `bytes`. The hash is result evidence. Denial, failure, or cancellation before
publication returns `executed: false`. If interrupted after publication, the
modified path and hash remain recorded with `executed: true` and
`status: "canceled"`.

## File boundary

The selected root is canonicalized and opened once. Each component is traversed
relative to the open directory with `O_NOFOLLOW`; all symlinks, including links to
in-root files, are rejected. Absolute paths, `..` components and `.git` access
are denied. Files must be regular and have one hard link, preventing writes to
outside aliases. FIFOs/devices cannot block a read. Parent/file identities and
contents are rechecked after approval before an atomic write. Keep external
editors and processes from concurrently moving project directories during a
session: this is not isolation from another hostile process running as your user.
A root change also prevents command execution.

## Verification policy and isolation

Bare names resolve in the fixed order `/usr/bin`, `/bin`, `/opt/homebrew/bin`.
Relative executable paths are rejected. An absolute path must resolve to an
executable in a documented system runtime directory. If the user supplies any repeatable `--allow-command JSON_ARGV`
options, the command must match one exact full argv, including the executable
string. Model calls cannot extend or rewrite the list. Every eligible invocation
shows its submitted argv and any distinct resolved executable path for separate
approval. Cognix rechecks the lookup and file identity before launch. It passes
the selected absolute invocation path and literal arguments directly, preserving
eligible aliases used by runtimes such as Python. There is no shell parsing,
expansion, or inherited stdin. The
sandbox also constrains explicitly chosen shells and interpreters. An
unsupported sandbox or sandbox launch failure never triggers an unsandboxed
retry.

After approval, every command checks the readable project tree before launch.
Any regular file with multiple hard links refuses the entire command with
`not_run` and `executed: false`, even when that file is absent from argv: a test
runner can open arbitrary project files. Use an ordinary copy instead of a hard
link. Seatbelt's path rules alone cannot distinguish a project file from an alias
to outside data or Git metadata. This check skips `.git` (already denied by
Seatbelt), never follows symlinks, and fails closed on inspection errors or more
than 100,000 entries. Inspection shares the command deadline and cancellation
flag. As with the file tools, concurrent hostile changes by another process
running as the same user are outside this boundary.

Commands refuse to run if the scratch directory would be inside the project.
They receive a fresh environment with a fixed PATH, scratch HOME/TMP,
disabled Python bytecode/user site, and disabled global/system Git configuration.
API keys, SSH agents, proxy settings, Python paths and other parent variables are
not inherited. Before exec on macOS, the forked child enumerates its actual file
descriptor table and marks every descriptor above stderr close-on-exec. Enumeration
or flag-setting failures abort launch; an incomplete table never permits execution.
Only redirected stdin/stdout/stderr survive exec, while Rust's spawn-error pipe
remains usable until exec succeeds or reports failure. The parent is unaffected.
The sandbox allows project and runtime reads, scratch writes,
and no network or project writes. Verification that needs dependency downloads,
project build output, child services, or user configuration can fail under this
policy; install dependencies separately and choose checks compatible with it.

- **macOS:** Seatbelt through `/usr/bin/sandbox-exec`, deny by default. Permit
  execution, runtime reads, metadata, root directory listing and sysctl reads.
  No Mach service lookups, network, process forks/spawns or external file-data
  reads. Disallowing descendants prevents a child from escaping cancellation
  through a new session/process group. Scratch files are the only writable
  directory; `/dev/null` is writable. `.git` is unreadable. `sandbox-exec` is a
  deprecated OS facility; unavailable/changed behavior fails closed.
- **Other platforms, including Linux:** command execution is unsupported and
  returns `not_run` before approval/launch. There is no unsandboxed fallback or
  unverified Linux backend. A future backend requires real-host isolation tests.

Runtime directories are `/usr`, `/bin`, `/sbin`, `/System`, `/Library/Apple`,
`/Library/Frameworks`, Homebrew's `Cellar`, `opt`, `lib`, and `bin`, when present.
These installed dependencies are trusted; do not keep private project data
there. A Python installed under a user's home is deliberately
not executable under this policy. On the evaluation Mac use
`/opt/homebrew/bin/python3`; Apple's `/usr/bin/python3` needs Xcode directories
that this policy does not expose. Sandbox isolation protects the filesystem and
network, not total resource consumption: commands can still consume memory/CPU
until their deadline. Review each proposed command before approval.

## Limits, cancellation and reporting

| Limit | Default / behavior |
| --- | --- |
| Model iterations | 16 per user task; `--max-steps` changes it. |
| Calls per model response | 8; excess/malformed envelope stops without batch execution. |
| Model request time | 120 seconds; `--model-timeout` (maximum 86,400 seconds); closes transport on cancellation/timeout. |
| Context | 16,384 via `--num-ctx` (minimum 8,192), checked against advertised model capacity. |
| Context accounting | Serialized UTF-8 history + schemas + 64/message + 4,096 reserved tokens; conservative byte-tokenizer budget. No truncation. |
| Generated response | 2,048 tokens; provider length exhaustion is a visible context-limit error. |
| Provider JSON response | 1 MiB; larger response rejected. |
| File size / patch result / new-file content | 64 KiB of UTF-8 bytes; oversized files rejected, not silently shortened. |
| Tool arguments | 1 MiB serialized JSON; closed fields and exact types; patch paths at most 4,096 bytes. |
| Command time | 30 seconds; `--command-timeout` (maximum 86,400 seconds). |
| Combined stdout/stderr | 16,384 bytes; `--output-limit`; excess kills execution and records `limit_reached`. |

Ctrl-C cancels prompts/model waits, kills running command execution and prevents
subsequent actions. HTTP cancellation closes the socket; the Ollama server may
finish internal inference after disconnect, but Cognix accepts no further work.
Command time/output limits kill execution, including a command that closes its
output pipes then keeps running. macOS denies child creation. Captured output
is returned to the model as data.
Terminal control and invisible Unicode formatting characters are escaped in
tool output, paths and diffs.

A process-wide Ctrl-C handler only sets an atomic cancellation flag. Rust
control flow is never unwound by SIGINT, including command spawn/cleanup and
the patch's atomic rename/bookkeeping and creation's atomic publish/bookkeeping.
Cancellation checks stop new actions
while ownership and evidence updates complete. A launched command is killed
and reaped and remains recorded as executed. An
approved patch or creation already committed at interruption remains in modified
files with
`executed: true` and a canceled result; cancellation does not undo approved edits.
Approval prompts keep their normal immediate Ctrl-C behavior.

The runtime's ledger determines modified files, exact checks, execution flags,
exit codes, failures, denials and unrun/canceled/limited work. The CLI displays
sanitized final model prose in a labeled response section so inspection answers
and blocker explanations are visible. A separate recorded execution evidence
section reports verification status from the ledger; model claims are not
verification evidence. The core retains the prose as `model_reply` and in its
conversation. A normal no-tool answer ends the turn, even if no files changed or
no check ran. Evidence remains explicit. All session history and evidence are in
memory, not written to disk by the product.
