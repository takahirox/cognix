# Cognix

Cognix is a local Ollama coding agent with a terminal interface, bounded tool
loops, approved edits, and sandboxed verification. The
[MVP specification](docs/mvp.md) defines the scope; [validation evidence](docs/validation.md)
records what has actually been tested for [Issue #6](https://github.com/takahirox/cognix/issues/6).

## Setup

Requires Python 3.11 or newer and a local [Ollama](https://ollama.com/) installation.
Runtime uses Python's standard library; setuptools builds the package. macOS
requires `/usr/bin/sandbox-exec` for commands. Linux requires bubblewrap (`bwrap`)
and permitted user namespaces. Other systems can inspect/edit files but cannot
run verification commands. Linux isolation has not yet been exercised on a real
Linux host; see the validation record.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -e '.[dev]'
ollama serve
# In another terminal, install a tool-capable local model if needed:
ollama pull qwen3-coder:30b
.venv/bin/cognix --help
```

The default model is `qwen3-coder:30b`. Set `COGNIX_MODEL` or pass `--model` to use
any compatible local tag, including suitable DeepSeek tags. Startup checks
`/api/version`, `/api/show`, and an actual tool-call probe. A missing model,
unsupported tools, failed probe, unavailable server, or cloud-backed tag stops
startup with an error. No hosted API is required. Only loopback HTTP Ollama
origins are accepted (`--ollama-url` or `COGNIX_OLLAMA_URL`).

**Qwen3.8 27B remains an evaluation target, not a dependency.** The local server
reported `qwen3.8:27b` missing during validation; its compatibility is unverified.
Do not infer compatibility from a model name. The available `qwen3-coder:30b`
passed the tool-call probe and three recorded fixture trials.

## Run

Start from a selected project directory using the installed executable:

```sh
/path/to/cognix/.venv/bin/cognix --model qwen3-coder:30b
```

Or select a project explicitly. This example is for Homebrew Python on Apple
Silicon; replace the executable with your absolute system-installed Python path:

```sh
.venv/bin/cognix --root /path/to/project --model qwen3-coder:30b \
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
.venv/bin/python -m unittest discover -s tests -v
.venv/bin/ruff check .
.venv/bin/ruff format --check .
.venv/bin/python scripts/check_docs.py
git diff --check
.venv/bin/python -m pip wheel --no-deps --wheel-dir dist .
```

The suite uses deterministic mock providers and a local HTTP fixture server; it
needs no model or hosted service. Sandbox integration tests run real subprocesses
and explicitly skip when the host lacks isolation or a compatible interpreter.
A skipped mandatory boundary check remains unverified on that host. Optional
real-model trials run separately:

```sh
.venv/bin/python scripts/evaluate_ollama.py --model qwen3-coder:30b \
  --python /opt/homebrew/bin/python3 --runs 3 --output /tmp/cognix-trials.json
```

This fixture-only evaluator approves `main.py` patches and its one exact test
command in disposable projects, recording every decision. It does not enable
automatic approvals in normal sessions.

- [Development flow](docs/development-flow.md)
- [Review and merge guidelines](docs/review-guidelines.md)
- [UI/UX and layout principles](docs/ui-ux-principles.md)
- [Test and CI performance principles](docs/testing-ci-principles.md)
- [Agent guidance](AGENTS.md)
