# Cognix

Cognix is a local Ollama coding agent with a terminal interface, bounded tool
loops, approved edits, and sandboxed verification. The
[MVP specification](docs/mvp.md) defines the scope; [validation evidence](docs/validation.md)
records the Rust migration checks for [Issue #8](https://github.com/takahirox/cognix/issues/8).

## Setup

The supported application is a native Rust executable on Apple Silicon macOS.
Use Rust/Cargo 1.87 or newer (validated with Homebrew Rust 1.94.0) and a local
[Ollama](https://ollama.com/) installation. Verification commands require
`/usr/bin/sandbox-exec`. Linux and other platforms are unsupported for command
execution and fail closed; no Linux isolation is claimed.

```sh
cargo build --locked --release
./target/release/cognix --help
# Optional installation into ~/.cargo/bin:
cargo install --locked --path .
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
Ollama 0.34.4. The target tag was explicitly downloaded for evaluation; normal
startup never downloads a model automatically. See the validation record for
exact model digests, observed results and platform limitations.

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

The model chooses `list_files`, `read_file`, `apply_patch`, and `run_command`.
Each patch displays a diff and exact replacement before `Approve this action?
[y/N]`; type `y` to approve. Commands show the exact argv and require separate
approval. Any other response, including EOF, denies the action. There is no
automatic approval option. Ctrl-C cancels work and exits with code 130. `/exit`
or EOF at the task prompt ends the session. `--task 'request'` runs one task while
retaining interactive approvals. Progress shows model waits, tool names and
structured result statuses.

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
