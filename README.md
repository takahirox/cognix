# Cognix

Cognix is a local Ollama coding agent with a terminal interface, bounded tool
loops, approved edits, and sandboxed project commands. The
[MVP specification](docs/mvp.md) defines the original scope; [validation evidence](docs/validation.md)
records the Rust migration and subsequent tool and security checks.
The [tool architecture decision](docs/tool-architecture.md) defines the future
general patch/command direction and incremental migration; it does not change
the currently supported tools or sandbox permissions. V2 remains a future
experimental contract with unchanged-content moves and read-only metadata-aware
previews; the read-only command wrapper can advance independently of patch model
trials. V1 remains available during experimental rollout.

## Setup

The supported application is a native Rust executable on Apple Silicon macOS.
Use Rust/Cargo 1.87 or newer (validated with Homebrew Rust 1.94.0) and a local
[Ollama](https://ollama.com/) installation. Verification commands require
`/usr/bin/sandbox-exec`. Linux and other platforms are unsupported for command
execution and fail closed; no Linux isolation is claimed.

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
output_limit = 16384
```

Explicit CLI flags take priority over `COGNIX_MODEL` and `COGNIX_OLLAMA_URL`,
then the config file, then the built-in defaults. For example, run
`cognix --model qwen3.8:27b` to override a saved model for one session.
Configuration errors identify the file and the problem.

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
are literal. The macOS sandbox allows project reads and scratch writes, and
blocks network access and project writes. Checks that need subprocesses, builds
that write into the project, or tools outside the permitted runtime may fail.

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

Commands show the submitted argv and any distinct executable path. Type `y`
at `Approve this action? [y/N]` to approve an individual command or patch.
Other responses, including EOF, deny it. Use repeatable
`--allow-command JSON_ARGV` flags to restrict commands to exact submitted arrays.
For example, `--allow-command '["cat","src/main.rs"]'` permits that exact proposal
to reach approval. Ctrl-C cancels work and exits with code 130. Use `/exit` or EOF
at the task prompt to end the session, or `--task 'request'` for one task with
interactive approvals.

Results record modified files and commands with their exit codes and output.
They distinguish `passed`, `failed`, `not_run`, `denied`, `canceled`, and
`limit_reached`. A `passed` command exited with code 0. Use the recorded test
command and its output to identify which tests ran. In single-task mode, any
failed or denied action makes the exit code 1, even if a later action succeeds.

### Verify a fix

From the Cognix checkout, copy the division fixture into a temporary project
and launch Cognix there.

```sh
project=$(mktemp -d)
cp tests/fixtures/division/*.py "$project/"
cd "$project"
cognix
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
`/opt/homebrew/bin/python3` and fail if that boundary is unavailable. They cover
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
and creation of `test_division_edges.py`. Normal sessions require explicit approval.

- [Development flow](docs/development-flow.md)
- [Review and merge guidelines](docs/review-guidelines.md)
- [UI/UX and layout principles](docs/ui-ux-principles.md)
- [Test and CI performance principles](docs/testing-ci-principles.md)
- [Agent guidance](AGENTS.md)
