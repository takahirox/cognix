# Development launcher validation

## Model provisioning (Issue #12)

Validated on 2026-11-08 on Apple Silicon macOS 27.2, system Bash 3.2.57,
Homebrew Rust/Cargo 1.94.0. This section records the current provisioning change;
the remaining sections preserve the earlier Issue #10 evidence.

`scripts/dev.sh` builds and runs the internal `cognix-dev-model` helper before
Cognix. The helper shares `src/cli.rs` with Cognix, avoiding a second model/default
or option-value parser. Rust's existing HTTP/JSON dependencies handle bounded
streaming, so no Python, jq or additional dependency is needed. Core agent and
provider behavior is unchanged. `cargo run` still defaults to `cognix`, and the
README's installation command installs only the application binary.

Model lookup uses `/api/show` with the original model selection. Only an HTTP
404 containing Ollama's specific missing-model error for that same model permits
consent. The comparison accounts for Ollama's implicit registry, library and
`:latest`, and case-insensitive canonical names; different tags remain distinct.
The response contract and name syntax were checked against Ollama's
[0.34.4 server handler](https://github.com/ollama/ollama/blob/v0.34.4/server/routes.go)
and [name validation](https://github.com/ollama/ollama/blob/v0.34.4/types/model/name.go).
Other errors and incompatible payloads fail closed. Successful metadata rejects
remote model/host fields. Pulling and the final availability check target the
same validated loopback origin, without proxies or redirects.

### Provisioning acceptance

| Criterion | Evidence | Result |
| --- | --- | --- |
| AC1 — Selection/presence | Real HTTP fixtures verify space/equal CLI forms, CLI over environment, environment/default selection, namespace/registry and implicit-tag normalization, exact request model, quoted/newline/JSON argv and retained stdin. Installed models produce no prompt/pull; a cached rerun does not update. | Pass. |
| AC2 — Consent | Disposable pseudoterminals exercise explicit approval, No, blank/default No and terminal EOF. Null/piped input, including piped `yes`, cannot approve. Missing-model messages show the tag, endpoint, network/disk warning and endpoint-specific manual command; declined runs do not start Cognix. | Pass. |
| AC3 — Download | Custom-port fixtures record streaming `/api/pull` for the selected tag, then `/api/show` before launching. A 2000-record stream yields bounded status/byte output. Native Cognix accepts a compatible downloaded model and rejects unsupported capabilities or a failed actual tool-call probe. | Pass with disposable fixtures. |
| AC4 — Safety/failure | Tests cover invalid/empty/cloud tags, remote metadata, authentication/server errors, unrelated 404s, wrong-model 404s, redirects, malformed JSON/metadata, refused model connections, disk/network pull errors, incomplete/invalid streams, success without availability and a separate one-second download timeout. SIGINT/SIGTERM cancel a real pending pull; no failed case starts Cognix or selects a hosted fallback. | Pass. |
| AC5 — Lifecycle | Existing Issue #10 regressions still pass. Added provisioning failure and interrupt paths preserve helper/build/Cognix exit status, owned Ollama/descendant cleanup and retained logs; existing servers remain unowned. Pseudoterminals verify consent does not consume later approval input. | Pass. |
| AC6 — Tests/docs | Commands below, updated README setup/launcher/manual fallback and this record. Fixtures require no model download and never bind the developer's default Ollama port. | Pass. |

### Provisioning check results

| Command/check | Result |
| --- | --- |
| `/bin/bash -n scripts/dev.sh` | Pass with system Bash 3.2. |
| `cargo test --locked --test dev_launcher` | Pass: 18 tests (17 behavior tests plus native-child fixture), no failures or ignored tests. |
| `cargo test --locked --all-targets` | Pass: 58 tests, no failures or ignored tests, including real macOS sandbox and CLI signal coverage. Final helper hardening and provisioning-failure coverage were checked again with the launcher suite. |
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass. |
| `cargo build --locked --release` | Pass for both binaries. |
| `cargo run --locked --example check_docs` | Pass: 13 Markdown documents, zero errors. |
| `git diff --check` and staged equivalent | Pass. |
| ShellCheck | Not run: unavailable; Bash syntax and behavioral coverage ran. |

### Provisioning limits and optional validation

No live model was downloaded and no existing model cache was modified. The
optional live first-install/cached smoke test was not run; HTTP fixtures simulate
Ollama model state and progress, including errors. They prove endpoint targeting,
consent, parsing and lifecycle behavior, but do not claim a real registry,
storage-exhaustion, macOS Ollama app or model-compatibility trial. A canceled
request may leave Ollama-managed partial data; the launcher never deletes it.
Non-macOS launcher behavior remains unsupported and these launcher tests run only
on macOS. Download timeout defaults to 3600 seconds and is configurable separately
from server startup via `COGNIX_DEV_PULL_TIMEOUT` (1–86400 seconds).

Required post-merge verification: none required.

## Earlier server startup (Issue #10)

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
