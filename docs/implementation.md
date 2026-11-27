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
publication. Updates atomically exchange the staged and destination files,
then verify the displaced file before committing. A concurrent save causes a
conflict and is restored when possible. If a further change prevents safe
restoration, Cognix reports where to find the preserved entry. Creations use
exclusive `linkat`, which refuses a concurrently created destination. A new
file has mode 0600, subject to umask. Staging cleanup checks inode ownership.

Success returns `status: "ok"`, `executed: true`, normalized `path`, `sha256`,
and `bytes`. The hash is result evidence. Denial, failure, or cancellation before
commit returns `executed: false`. If interrupted after commit, the
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

In manual and auto modes, bare names resolve in the fixed order `/usr/bin`,
`/bin`, `/opt/homebrew/bin`. Absolute invocations must resolve to installed
executables under a trusted runtime directory. The readable runtime directories
are `/usr`, `/bin`, `/sbin`, `/System`, `/Library/Apple`,
`/Library/Frameworks`, `/Library/Developer/CommandLineTools`,
`/Applications/Xcode.app/Contents/Developer`, and Homebrew's `Cellar`, `opt`,
`lib`, and `bin`, when present. Developer-tool shims can depend on Xcode
services or frameworks outside those locations. Use an eligible absolute path
to the installed program when a shim fails.

The optional `--allow-command` policy checks the full submitted argv before
review. The runtime binds approval to the exact prepared action and effective
scope, checks the root and executable identity again, and passes literal
arguments with null stdin. A bounded project scan rejects hard-link aliases,
skips `.git`, avoids symlink traversal and shares the command deadline.
Inspection errors or more than 100,000 entries stop launch. Simultaneous hostile
changes from another same-user process remain outside this boundary.

Seatbelt denies external file data, network connections and Mach service
lookups by default. Commands read the project and installed runtimes, with
private scratch for HOME and temporary files. The environment is rebuilt and
inherited descriptors above stderr are marked close-on-exec. Both profiles
protect Git metadata and outside files. `workspace-write` additionally permits
live workspace writes and subprocesses under the same Seatbelt policy.
`read-only` keeps the workspace read-only to commands and disallows descendants.
Approved patch publication uses its own descriptor-relative boundary in both
profiles.

The scoped network transport accepts a single HTTP(S) GET from private scratch,
using a fixed system client and Cognix-owned options. Configuration, stdin,
workspace paths, proxy settings, URL globbing and redirect following are absent
from this transport. The URL submitted for approval identifies the HTTP origin
by scheme, host and port. DNS resolution and TLS use the system transport.
Origin denials apply before any approval. Arbitrary network clients remain
inside the network-denied Seatbelt profile.

When approval mode is `auto`, the approval boundary prepares an independent
local Ollama request with a security instruction and bounded structured
evidence. It offers the reviewer zero tools. A strict JSON verdict allows or
denies the prepared action, or sends it to the same default-No confirmation
used in manual mode. Review failures, timeouts and oversize contexts also use
that prompt. When canceled, the request returns without prompting or committing.
The review result includes a reason and a digest of the prepared action and
policy. Manual confirmations recorded by the runtime travel separately from
assistant claims and untrusted tool output.
[Ollama structured chat output](https://github.com/ollama/ollama/blob/v0.34.4/docs/api.md#chat-request-structured-outputs)

`sandbox-exec` is a deprecated macOS facility. An unavailable or failed sandbox
never triggers an unsandboxed retry. Explicit full access disables command
isolation and ordinary approval and retains usual OS restrictions. The command
time/output limit sends a signal to the launched process group. Deliberately
detached descendants can outlive group cleanup in workspace-write or full
access. In workspace-write they retain the same filesystem and network
restrictions. Seatbelt provides no total CPU, memory, disk or process quota.

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
Command time/output limits terminate the launched process group, including a
command that closes its output pipes then keeps running. Captured output
is returned to the model as data.
Terminal control and invisible Unicode formatting characters are escaped in
tool output, paths and diffs.

A process-wide Ctrl-C handler only sets an atomic cancellation flag. Rust
control flow is never unwound by SIGINT, including command spawn/cleanup,
patch exchange and verification, and creation's atomic publish/bookkeeping.
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
