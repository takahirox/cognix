# Cognix

Cognix is a local Ollama coding agent with a terminal interface, bounded tool
loops, approved edits, and sandboxed verification. The
[MVP specification](docs/mvp.md) defines the original scope; [validation evidence](docs/validation.md)
records the Rust migration and subsequent tool and security checks.

## Setup

The supported application is a native Rust executable on Apple Silicon macOS.
Use Rust/Cargo 1.87 or newer (validated with Homebrew Rust 1.94.0) and a local
[Ollama](https://ollama.com/) installation. Verification commands require
`/usr/bin/sandbox-exec`. Linux and other platforms are unsupported for command
execution and fail closed; no Linux isolation is claimed.

```sh
./scripts/dev.sh --model qwen3-coder:30b
```

The development launcher starts local Ollama if needed and offers to download a
missing model with explicit confirmation. See [development launcher](#development-launcher)
for consent, timeout and manual-installation behavior. To build and run the direct
binary instead:

```sh
cargo build --locked --release
./target/release/cognix --help
# Optional installation into ~/.cargo/bin:
cargo install --locked --path . --bin cognix
ollama serve
# In another terminal, explicitly install a model only if needed:
ollama pull qwen3-coder:30b
```

Cargo downloads build dependencies on the first build; the committed lockfile
pins their versions. The application needs no Python runtime. The repository's
small Python fixture and macOS security tests use Homebrew Python separately as
the approved verification program, not as the Cognix application runtime.

The default model is `qwen3-coder:30b`. Set `COGNIX_MODEL` or pass `--model` to use
any compatible local tag, including suitable DeepSeek tags. Startup checks
`/api/version`, `/api/show`, and an actual tool-call probe. A missing model,
unsupported tools, failed probe, unavailable server, or cloud-backed tag stops
startup with an error. No hosted API is required. Only loopback HTTP Ollama
origins are accepted (`--ollama-url` or `COGNIX_OLLAMA_URL`).

**Qwen3.8 27B is an evaluated target, not a dependency.** Both `qwen3.8:27b`
and `qwen3-coder:30b` passed the tool-call probe and three fixture trials each on
Ollama 0.34.4. The target tag was explicitly downloaded for evaluation; the direct
`cognix` binary never downloads models. The launcher asks before any download.
See the validation record for exact model digests, observed results and platform
limitations.

## Development launcher

On Apple Silicon macOS, install Rust/Cargo 1.87+ and Ollama, with `cargo` and
`ollama` on `PATH`. The launcher uses the macOS-provided Bash, curl and plutil;
it needs no Python runtime. Run:

```sh
./scripts/dev.sh --model qwen3.8:27b
./scripts/dev.sh --root '/path/to/my project' --task 'Inspect main.py' \
  --allow-command '["/usr/bin/true"]'
# From another directory (the current directory remains the default project):
/path/to/cognix/scripts/dev.sh --model qwen3-coder:30b
```

The script is committed executable; if a copy lost that permission, run
`chmod +x scripts/dev.sh`. It builds the native debug CLI and its internal model
provisioning helper using the repository's Cargo manifest and `target` directory,
forwarding all CLI arguments unchanged.
`COGNIX_MODEL`, `COGNIX_OLLAMA_URL` and the `--ollama-url` override still apply.

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

After building, the helper uses Cognix's own CLI parser to resolve `--model tag`
or `--model=tag`, then `COGNIX_MODEL`, then `qwen3-coder:30b`. It checks that exact
selection through `/api/show` on the configured endpoint. Installed models start
without prompts, updates or downloads. Help/version and invalid CLI options are
handled by the same parser without provisioning.

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
install a suitable model using the command above, then run
`cargo run --locked --manifest-path /path/to/cognix/Cargo.toml -- --model qwen3.8:27b`
in another, or use the built executable described below. See the
[launcher validation record](docs/dev-launcher-validation.md) for focused checks.

## Run

Start from a selected project directory using the installed executable:

```sh
/path/to/cognix/target/release/cognix --model qwen3-coder:30b
```

Or select a project explicitly. This example is for Homebrew Python on Apple
Silicon; replace the executable with your absolute system-installed Python path:

```sh
./target/release/cognix --root /path/to/project --model qwen3-coder:30b \
  --allow-command '["/opt/homebrew/bin/python3","-I","-B","-m","unittest","discover","-s",".","-p","test_*.py"]'
# > Fix the division-by-zero bug in main.py; return None for zero and verify.
```

The model chooses `list_files`, `read_file`, `apply_patch`, `create_file`, and `run_command`.
Each patch displays a diff and exact replacement before `Approve this action?
[y/N]`; type `y` to approve. Commands show the exact argv and require separate
approval. Any other response, including EOF, denies the action. There is no
automatic approval option. Ctrl-C cancels work and exits with code 130. `/exit`
or EOF at the task prompt ends the session. `--task 'request'` runs one task while
retaining interactive approvals. Progress shows model waits, tool names and
structured result statuses.

`create_file` takes a project-relative `path` and complete UTF-8 `content` (up to
64 KiB, including empty text; no NULs). It displays the destination, byte count
and a bounded add-only preview before the same per-action approval prompt.
The parent directory must already exist. Absolute paths, `..`, `.git`, symlinked
parents and existing destinations are rejected; publication never overwrites,
even if a destination appears during approval. It creates one regular file with
owner-only permissions (0600, subject to umask), and records it in modified files
and session evidence. Denial, EOF or cancellation before publication leaves no
new file. Cancellation after publication preserves the approved file and its
execution evidence. Creating directories, deleting and renaming remain unsupported;
`apply_patch` still requires reading an existing file first.

Results list modified files and exact checks, exit codes and captured output.
They distinguish `passed`, `failed`, `not_run`, `denied`, `canceled` and
`limit_reached`. No command means verification was not run. A zero exit code is
recorded as passed; it does not prove test coverage or task correctness. Final
model responses are sanitized and displayed separately from recorded execution
evidence, which determines check statuses. A no-tool response ends a turn, not
proof of success.
In single-task mode, any failed/denied action makes the exit code 1 even if a
later correction succeeds; inspect the check ledger for recovery evidence.

For a reproducible example, copy `tests/fixtures/division` into a temporary project
and use the command above. Its zero-denominator test intentionally fails until
`main.py` returns `None`. See [development and security details](docs/implementation.md)
for the exact tool contract, numeric limits, execution policy and limitations.

## Check and build

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

This fixture-only evaluator approves `main.py` patches and its one exact test
command in disposable projects, recording every decision. It does not enable
automatic approvals in normal sessions.

- [Development flow](docs/development-flow.md)
- [Review and merge guidelines](docs/review-guidelines.md)
- [UI/UX and layout principles](docs/ui-ux-principles.md)
- [Test and CI performance principles](docs/testing-ci-principles.md)
- [Agent guidance](AGENTS.md)
