# MVP implementation

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
| `list_files` | `path` | One directory; `.` means root. Symlinks and `.git` omitted. |
| `read_file` | `path` | UTF-8 regular file, content and SHA-256; no binary/NUL data. |
| `apply_patch` | `path`, `sha256`, `old_text`, `new_text` | Previously read existing file; replace one unique nonempty span after preview/approval. |
| `run_command` | `argv` | Exact allowlisted argument array, approval, sandbox, bounded result. |

Patching intentionally supports existing files only, not creation, deletion,
renaming, or whole-project patches. `old_text` must occur exactly once. Reads
record hashes; changes since a read or during approval reject the patch. An
atomic same-directory replacement preserves content outside the requested span
and ordinary mode bits. No Git reset/checkout or cleanup touches user edits.
Extended attributes and ACLs are not copied by atomic replacement; use this MVP
on ordinary source files. Approved edits remain after cancellation or a failed
check, and are reported; there is no automatic rollback.

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

No commands are enabled by default. The user supplies repeatable
`--allow-command JSON_ARGV` options at startup. Each must contain an existing
absolute executable in a system runtime directory; policy comparisons use the
exact full argv, including the executable string. Model calls cannot extend or
rewrite this list. There is no shell parsing or expansion and no inherited stdin.
An explicitly chosen shell or interpreter is still constrained by the sandbox.
Approval is required even for an allowlisted invocation. An unsupported sandbox
or sandbox launch failure never triggers an unsandboxed retry.

Commands receive a fresh environment with a fixed PATH, scratch HOME/TMP,
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
until their deadline. Keep the allowlist small and review each action.

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
| File size / patch result | 64 KiB; oversized files rejected, not silently shortened. |
| Directory listing | 500 entries and the output byte budget; choose a narrower directory on failure. |
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
the patch's atomic rename/bookkeeping. Cancellation checks stop new actions
while ownership and evidence updates complete. A launched command is killed
and reaped and remains recorded as executed. An
approved patch already committed at interruption remains in modified files with
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
