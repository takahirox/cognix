# Development launcher validation

Validation for [Issue #10](https://github.com/takahirox/cognix/issues/10), performed
on 2026-11-08 on Apple Silicon macOS 27.2 (Darwin arm64), with macOS Bash
3.2.57, Homebrew Rust/Cargo 1.94.0 and Ollama 0.34.4. This record covers the
development launcher; the [existing validation](validation.md) describes the
core application and its earlier model evaluations.

## Mandatory acceptance

| Criterion | Evidence | Result |
| --- | --- | --- |
| AC1 — One-command launch | `scripts/dev.sh` resolves its repository, builds through its Cargo manifest, and runs the native CLI while keeping the caller's project directory. `tests/dev_launcher.rs` checks paths with spaces, Cargo argv/cwd, verbatim CLI argv (including quotes, whitespace, newlines and JSON), environment defaults and interactive stdin. Real native CLI launches with disposable HTTP fixtures run from the repository and another directory. README documents invocation and executable permissions. | Pass. |
| AC2 — Existing Ollama | Real loopback HTTP fixtures are reused by the native CLI across successive launcher invocations; no `ollama` process is started. CLI endpoint overrides take precedence over the environment. Signal tests also verify the existing-server path never starts or owns Ollama. | Pass with fixtures; live app-managed server test not run. |
| AC3 — Managed startup | Disposable `ollama serve` stub and native descendants exercise refused connection → readiness → Cognix → cleanup. The stub checks explicit default `OLLAMA_HOST` binding even with a conflicting inherited value. A live installed Ollama/native CLI smoke test also passed, as described below. | Pass. |
| AC4 — Failure paths | Focused tests cover missing `ollama`, refused custom endpoint, nonlocal URL, invalid/non-JSON/plist/empty or non-string version responses, HTTP redirects, unresponsive listeners, early daemon exit, bounded readiness timeout and build failure. Failure logs remain available. Real native CLI with HTTP fixtures reports missing models and unsupported capabilities, preserving exit 1; requests are limited to version/show, with no pull or fallback. Owned processes are verified stopped. | Pass. |
| AC5 — Lifecycle/status | Tests preserve a normal Cognix exit of 23 and build exit of 17. SIGINT/SIGTERM during Cognix, SIGHUP during build, and SIGTERM during startup produce 130/143/129 as appropriate. Owned server groups and their native descendants stop; the existing-server path owns no server. The native CLI retains stdin for approvals, and existing CLI SIGINT/sandbox regression tests pass. | Pass. |
| AC6 — Tests/docs | Focused launcher coverage, full Cargo regression suite, Bash syntax, formatting, Clippy, documentation checker, release build and whitespace checks listed below. README covers prerequisites, examples, logs, reuse and manual startup. | Pass. |

The focused suite uses isolated repository copies, stub Cargo/Ollama/curl
commands, disposable native child processes and real ephemeral loopback HTTP
fixtures. It never binds the developer's default port for automated tests.
The native CLI fixtures cover the actual provider/model checks without downloading
models or changing provider, harness, sandbox or approval policy.

## Check results

| Command/check | Result |
| --- | --- |
| `/bin/bash -n scripts/dev.sh` | Pass on the system Bash 3.2. |
| `cargo test --locked --test dev_launcher` | Pass: 8 tests, including 7 behavior tests and the disposable native-child entry point; no failures or ignored tests. |
| `cargo test --locked --all-targets` | Pass: 48 tests, no failures or ignored tests, including real macOS sandbox and CLI signal checks. |
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass. |
| `cargo build --locked --release` | Pass; optimized build completed on this Mac. |
| `cargo run --locked --example check_docs` | Pass: 13 Markdown documents, zero errors. |
| `git diff --check` and staged equivalent | Pass. |
| ShellCheck | Not run: not installed; system Bash syntax and behavioral tests ran instead. |

Initial tests caught Bash 3.2's empty-array behavior under `set -u` and macOS
path aliases; the launcher now preserves positional arguments directly, and the
fixture compares canonical paths. Clippy also caught an unwaited disposable
fixture child; the fixture now waits for it. Final results above include these
corrections.

## Optional live smoke test and limits

Pass: the default endpoint refused connections before the smoke test.
`./scripts/dev.sh --help` then started installed Ollama 0.34.4, validated its
version response, built Cognix through the real Cargo manifest, printed native
CLI help and exited 0. The printed temporary `ollama.log` was retained; afterward,
a direct `/api/version` request again failed with connection refused (curl exit
7), confirming that the owned live server was stopped. No model was pulled or
inference task requested.

Not run: manual testing with the macOS Ollama app already running, live model
inference through the launcher, and terminal-driven Ctrl-C with a live model.
Disposable HTTP/process tests cover reuse, model failures and cancellation;
these do not claim a live model or app-managed lifecycle test. No new model
compatibility or cross-platform guarantee is claimed.

Required post-merge verification: none required.
