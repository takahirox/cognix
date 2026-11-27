# Cognix

Cognix is a local Ollama coding agent with a terminal interface, bounded tool
loops, approved edits, and sandboxed project commands. The
[MVP specification](docs/mvp.md) defines the original scope; [validation evidence](docs/validation.md)
records the Rust migration and subsequent tool and security checks.
The [tool architecture decision](docs/tool-architecture.md) covers supported command permissions and the roadmap for the experimental v2
patch contract.

## Setup

The supported application is a native Rust executable on Apple Silicon macOS.
Use Rust/Cargo 1.87 or newer (validated with Homebrew Rust 1.94.0) and a local
[Ollama](https://ollama.com/) installation. Sandboxed commands require
`/usr/bin/sandbox-exec` on macOS. Sandboxed command proposals fail closed on
other platforms.

Install Cognix from this checkout. Make sure Cargo's bin directory
(`~/.cargo/bin` by default) is on your `PATH`.

```sh
cargo install --locked --path . --bin cognix
```

Open the Ollama app or run `ollama serve` in a separate terminal. Install the
default model if you need it, then start Cognix from your project directory.

```sh
ollama pull qwen3-coder:30b
cd /path/to/project
cognix
```

The [development launcher](#development-launcher) can start local Ollama and
offers to download a missing model with explicit confirmation.

Cargo downloads build dependencies on the first build; the committed lockfile
pins their versions. The application needs no Python runtime. The repository's
small Python fixture and macOS security tests use Homebrew Python separately as
the approved verification program, not as the Cognix application runtime.

Startup checks `/api/version`, `/api/show`, and an actual tool-call probe. A missing model,
unsupported tools, failed probe, unavailable server, or cloud-backed tag stops
startup with an error. No hosted API is required. Only loopback HTTP Ollama
origins are accepted (`--ollama-url` or `COGNIX_OLLAMA_URL`).

### User configuration

Save settings in `~/.cognix/config.toml`. Each top-level key is optional. This
example lists the built-in defaults. Both timeouts are in seconds, and
`output_limit` caps combined command output in bytes.

```toml
model = "qwen3-coder:30b"
ollama_url = "http://127.0.0.1:11434"
num_ctx = 16384
max_steps = 16
model_timeout = 120
command_timeout = 30
approval_mode = "manual"
sandbox_mode = "workspace-write"
output_limit = 16384
```

Explicit CLI flags take priority over `COGNIX_MODEL` and `COGNIX_OLLAMA_URL`,
then the config file, then the built-in defaults. For example, run
`cognix --model qwen3.8:27b` to override a saved model for one session.
Configuration errors identify the file and the problem.

To enable automatic review, add these settings. `review_model` is optional.
When omitted it inherits the effective working model, including a `--model`
override. A separate reviewer needs local completion support. The launcher
continues to provision only the working model, with its existing consent flow.

```toml
model = "qwen3.8:27b"
approval_mode = "auto"
review_model = "qwen3.8:27b"
sandbox_mode = "workspace-write"
deny_network = ["https://example.com:8443"]
```

Use `--approval-mode`, `--sandbox-mode`, or `--review-model` to override
the corresponding saved setting for one session. `deny_network` defaults to
an empty list. Each entry is an HTTP(S) origin including the port when it differs
from the protocol default.

**Qwen3.8 27B is an evaluated target, not a dependency.** Both `qwen3.8:27b`
and `qwen3-coder:30b` passed the tool-call probe and three fixture trials each on
Ollama 0.34.4 using the earlier tool interface. The target tag was explicitly
downloaded for evaluation. The direct `cognix` binary never downloads models.
The launcher asks before any download.
See the validation record for exact model digests, observed results and platform
limitations.

## Development launcher

On Apple Silicon macOS, install Rust/Cargo 1.87+ and Ollama, with `cargo` and
`ollama` on `PATH`. The launcher uses the macOS-provided Bash, curl and plutil;
it needs no Python runtime. Run:

```sh
./scripts/dev.sh --model qwen3.8:27b
./scripts/dev.sh --root '/path/to/my project' --task 'Inspect main.py'
# From another directory (the current directory remains the default project):
/path/to/cognix/scripts/dev.sh --model qwen3-coder:30b
```

The script is committed executable; if a copy lost that permission, run
`chmod +x scripts/dev.sh`. It builds the native debug CLI and its internal model
provisioning helper using the repository's Cargo manifest and `target` directory,
forwarding all CLI arguments unchanged.
It uses the same settings as the installed executable.

An existing server with a valid Ollama `/api/version` response is reused and
left running. If the default `http://127.0.0.1:11434` endpoint refuses connections,
the launcher starts `ollama serve` bound to that address, waits up to 30 seconds,
and stops only its own server/process group on exit or interruption. Custom local
HTTP endpoints must already be running; the launcher never starts a daemon for
them. An incompatible or unresponsive listener produces an error. Override the
readiness deadline with `COGNIX_DEV_STARTUP_TIMEOUT` (1–300 seconds) if necessary.
Startup prints the path to a retained temporary `ollama.log`; inspect it with
`cat /printed/path/ollama.log`, then delete that temporary directory when done.
Normal Cognix exit codes pass through; Ctrl-C, termination and hangup exit with
130, 143 and 129 respectively, with bounded cleanup of owned processes.

After building, the helper resolves the endpoint and model with Cognix's CLI
parser. It checks that exact model through `/api/show` on the configured
endpoint. Installed models start without prompts, updates or downloads.
If effective settings change between the endpoint probe, model provisioning
and CLI startup, the launcher stops and asks you to run it again.
Help/version and invalid CLI options are handled by the same parser without
provisioning.

Only Ollama's explicit missing-model response offers a download. The launcher
shows the tag, endpoint and warning about potentially many GB of network/disk
usage, then asks `Download <tag>? [y/N]`. Type `y` or `yes` to approve. Blank input,
No, EOF or non-terminal stdin exits 1 without fetching or starting Cognix. Piping
`yes` does not approve a download. The error includes a manual alternative:

```sh
# Start the chosen local server first, then target that same instance:
OLLAMA_HOST='http://127.0.0.1:11434' ollama pull qwen3.8:27b
```

Approved downloads use streaming `/api/pull` on that same loopback endpoint,
with bounded status/byte progress and a local availability check afterward.
`COGNIX_DEV_PULL_TIMEOUT` sets the separate total download deadline (1–86400
seconds; default 3600). Ctrl-C cancels provisioning. Failure, timeout or
cancellation stops startup; inspect the reported error and check network access
and free disk space before retrying. Ollama may retain partial data for a retry;
the launcher never deletes model caches or user data. Invalid names, cloud-backed
tags/metadata, authentication failures, incompatible responses and other server
errors do not trigger downloads. No hosted inference fallback is selected.
Network access to model registries is needed only for an approved download;
normal local inference requires no hosted AI API.

Cognix still checks local model readiness, capabilities and actual tool calling,
and retains its approval, sandbox and command allowlist behavior. A downloaded
model can fail these checks. The direct `cognix` binary does not provision models.
To run manually, start `ollama serve` in one terminal (or use the Ollama app),
install a suitable model, then launch the installed executable as described in
[Setup](#setup). See the [launcher validation record](docs/dev-launcher-validation.md)
for focused checks.

## Run

Start `cognix` in a project directory or select it with `cognix --root /path/to/project`.

Cognix uses two tools. `run_command` lists files, reads source, searches text,
and runs checks from the project root. For example, it can submit
`["ls", "-1"]`, `["cat", "src/main.rs"]`, or `["grep", "-n", "fn main", "src/main.rs"]`.
Use `rg` if installed in an eligible runtime directory. Bare command names are
resolved in `/usr/bin`, `/bin`, and `/opt/homebrew/bin`, in that order. Use an
eligible absolute path to select a different installed executable. Arguments
are literal.

`apply_patch` creates or updates one UTF-8 file at a time using a project-relative
`path`. For an update, supply an exact, unique `old_text` span and its
`new_text` replacement. An empty `old_text` updates an empty existing file.
For creation, set `old_text` to `null` and supply the complete file in
`new_text`, which may be empty.

```json
{"path":"src/new_module.rs","old_text":null,"new_text":"pub fn answer() -> u32 { 42 }\n"}
```

Each patch shows a diff or a bounded add-only preview. Cognix snapshots an
existing file while preparing the preview and checks its contents and identity
again before writing. Creation requires an existing directory and refuses to
overwrite any destination. Files are limited to 64 KiB of UTF-8 without NULs.
Absolute patch paths, traversal, `.git`, and symlinks are refused. File deletion,
moves, directory creation, and multi-file patches are unsupported.

### Approval and permissions

- **Manual (default).** Cognix shows the command and its scope or the patch
  preview. Type `y` at `Approve this action? [y/N]` to approve that action.
  Other responses and EOF leave the action unexecuted.
- **Auto.** A separate local Ollama request reviews the exact prepared action.
  It receives the user instructions, recorded human approvals, and labeled
  excerpts of untrusted conversation and tool output. An allow applies only
  to that action within its enforced scope. A denial stops further tool actions
  for the current task. Uncertain, invalid, unavailable, oversized, or timed-out
  reviews ask for manual confirmation. The model can misjudge intent or risk.
  [Reviewer evaluation](docs/validation.md#issue-30-approval-and-sandbox-validation)
- **Full access.** `--approval-mode full-access` or the corresponding saved
  setting explicitly selects this mode and prints a startup warning. Commands
  run with your environment and ordinary OS access to files and network.
  The patch tool retains its project-relative contract. Full access is never
  selected by a reviewer or a fallback.

In manual and auto modes, fixed no-argument system `ls` and `pwd` can run
without a prompt. Other commands, including tests and builds, require approval.
Repeat `--allow-command JSON_ARGV` to restrict commands to exact submitted
arrays, including the executable spelling. An allowlist entry still follows
the approval mode and sandbox policy.

In the default `workspace-write` profile, commands and their children can
write directly to the live project and private scratch space. The sandbox
blocks external file data apart from installed runtimes, protects `.git`, and
blocks direct network access. Workspace-write supports children created with
fork and exec. A shell build script or a system Ruby build and test task such as
`ruby -S rake` can run within the contained group. The profile blocks
`posix_spawn` and process group changes so descendants stay within cancellation
scope. Tools that require `posix_spawn`, including Homebrew Cargo, Apple's Make
and Homebrew's Python launcher, are incompatible with this profile.
`read-only` permits command writes only to scratch and prevents child
processes. Approved `apply_patch` calls can write
in either profile. Check
[the sandbox details](docs/implementation.md#verification-policy-and-isolation)
for the readable runtime paths and process limits.

In either profile, exactly `["curl","https://host/path"]` (or the equivalent
`/usr/bin/curl` invocation) requests one brokered HTTP(S) GET. Cognix shows or
reviews the URL, host, port and protocol, including any query data sent by the
request. A configured `deny_network` entry blocks the corresponding origin
before review. A refused GET blocks retries of that URL for the session.
Other URLs, including those on the same origin, receive their own approval
checks. The broker returns redirect responses for inspection and leaves the
redirected URL unrequested.

Build and test scripts that fetch files can opt in to the private broker with
`"http_get": true` alongside the command's `argv`. A running script can request
an exact URL through the Unix socket identified by `COGNIX_HTTP_SOCKET`.

```sh
/usr/bin/curl --disable --fail --unix-socket "$COGNIX_HTTP_SOCKET" \
  --request-target "$artifact_url" http://localhost/ -o artifact
```

Each GET has its own confirmation in manual mode or review in auto mode.
Pending confirmation ends when the parent command exits or times out.
The configured origin denials apply in both modes. The private connection
returns the upstream status and body (up to 4 MiB per request). Outbound
headers, uploads, tunnels, redirects and direct network access remain blocked.

Ctrl-C cancels work and exits with code 130. Use `/exit` or EOF at the task prompt
to end the session, or `--task 'request'` for one task. Reports record patch paths
and command exit codes and output. Commands with write access may also change
files directly, outside the patch ledger. In single-task mode, any failed or
denied action makes the exit code 1, even if a later action succeeds. This
includes brokered GET failures and denials, even when the parent command
handles them and exits with code 0.

### Verify a fix

From the Cognix checkout, copy the division fixture into a temporary project
and launch Cognix there.

```sh
project=$(mktemp -d)
cp tests/fixtures/division/*.py "$project/"
cd "$project"
cognix --sandbox-mode read-only
```

Paste the following as a single task at the prompt.

```text
List the files and inspect main.py and test_main.py. Run ["/opt/homebrew/bin/python3","-I","-B","-m","unittest","discover","-s",".","-p","test_*.py"] before making changes. Fix divide(a, 0) to return None, preserve the existing tests, and create test_division_edges.py with one unittest asserting divide(6, -2) == -3. Run the same command again and report the test results.
```

The example selects Homebrew Python on Apple Silicon. Review and approve the
exploration commands, patches, and test runs. The test command should initially
report `ZeroDivisionError` and exit 1. After the fix and the new test, it should
report three tests passing and exit 0. See the [implementation guide](docs/implementation.md)
for details on approval, limits, and sandbox policy.

## Check and build

The proposed v2 patch contract has an
[internal parser and read-only preview planner](docs/patch-planning.md).
V2 is an internal design experiment. The CLI uses the two tool contracts above.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo run --locked --example check_docs
git diff --check
cargo build --locked --release
```

Deterministic tests use scripted providers and loopback HTTP fixtures; no model
is needed. macOS integration tests execute real sandboxed subprocesses using
Homebrew Python and system executables. They cover
SIGINT in the CLI, model waits, committed patches, and running commands. Tests
for macOS execution are compiled only on macOS; their absence on another host is
not evidence of isolation. See the validation record for coverage and timings.
No CI or deployment workflow is established.

Real-model trials run separately:

```sh
cargo run --locked --example evaluate_ollama -- --model qwen3-coder:30b \
  --python /opt/homebrew/bin/python3 --runs 3 --output /tmp/cognix-trials.json
cargo run --locked --example evaluate_ollama -- --model qwen3.8:27b \
  --python /opt/homebrew/bin/python3 --runs 3 --output /tmp/cognix-target-trials.json
```

This fixture-only evaluator uses disposable projects and records every decision.
It approves exact fixture exploration and test commands, an update to `main.py`,
and creation of `test_division_edges.py`. To evaluate the reviewer alone on
prepared actions without executing them, run the local review trial:

```sh
cargo test --locked evaluate_local_reviewer -- --ignored --nocapture
```

- [Development flow](docs/development-flow.md)
- [Review and merge guidelines](docs/review-guidelines.md)
- [UI/UX and layout principles](docs/ui-ux-principles.md)
- [Test and CI performance principles](docs/testing-ci-principles.md)
- [Agent guidance](AGENTS.md)
