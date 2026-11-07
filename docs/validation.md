# Rust migration validation (Issue #8)

The initial migration results below apply to commit `9e90e6e` and its Cargo lockfile.
The raw model trials contain SHA-256 hashes of the implementation, evaluator,
documentation checker, manifests and fixture used. Their source hashes were
checked against that baseline. The descriptor-isolation revision has separate
evidence below; those earlier model trials were not rerun for the revision.
No PR, push, merge, comment or Issue closure was
performed by this implementation node. Review and integration remain downstream.

The [Python validation record](validation-python.md) and its two raw JSON records
are historical evidence only. They are not Rust test results.

## Review revision: inherited descriptor isolation (AC6)

On 2026-11-07 on the same Apple Silicon macOS environment, fixed command spawning
to mark all non-stdio descriptors close-on-exec in the forked child. The native
descriptor-table query covers handles above lowered soft/hard descriptor limits;
an enumeration failure, possibly truncated result or flag-setting failure aborts
launch. All allocation happens before fork, and the spawn-error pipe stays open
until exec so launch failures retain their normal reporting.

`tests/cli.rs::approved_command_cannot_use_inherited_outside_descriptors` starts
the actual CLI with low and high (at least 4096) inheritable handles to a synthetic
outside file, then lowers both descriptor limits to 256. A deterministic Ollama
fixture requests an allowlisted Python probe, approved through real CLI stdin.
Reads, writes and metadata access through both handles fail with `EBADF`; stdin
is empty, stdout/stderr are captured in order, the outside file is unchanged,
and recorded evidence reports execution with exit 0 and `passed`. This regression
was also run against the original launcher and failed because an outside handle
was accessible. With the fix it passes. A real-subprocess unit test additionally
checks that a truncated descriptor list aborts spawn and exec failure still
returns `ENOENT` through Rust's error pipe.

Revision checks: `cargo test --locked --all-targets` (37 passed, no failures or
skips, including real macOS sandbox/cancellation tests), rustfmt, Clippy with
warnings denied, the repository documentation checker and `git diff --check`
all passed. Local-model trials remain evidence for the baseline above, rather
than new observations of this revision. Non-macOS command support remains
unsupported and fail closed.

## Environment and reproduction

Evaluated on 2026-11-07: Apple M4 Max, 68,719,476,736 bytes (64 GiB) unified
memory, Darwin 27.2.0 arm64. Homebrew rustc 1.94.0
(`4a4ef493e`, host `aarch64-apple-darwin`), Cargo 1.94.0. The manifest requires
Rust 1.87 or newer; only 1.94.0 was exercised here. Application runtime: native
Rust, no Python. The external sandbox verification program is Homebrew
`/opt/homebrew/bin/python3` (3.15.2). Ollama server: **0.34.4**, bound to loopback.
The existing installation was started for this evaluation. No model download
was performed for the Rust migration.

The repository-owned fixture `tests/fixtures/division` is unchanged: a small
Python division function and two unittest cases. The zero-denominator case
intentionally fails until `main.py` returns `None`. Each trial copies it into a
fresh disposable project. Python is the fixture's verification program, not an
application dependency. No external dataset is used.

From the repository root:

```sh
cargo build --locked --release
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo run --locked --example check_docs
git diff --check
cargo run --locked --example evaluate_ollama -- --model qwen3-coder:30b \
  --python /opt/homebrew/bin/python3 --runs 3 --output /tmp/cognix-coder.json
cargo run --locked --example evaluate_ollama -- --model qwen3.8:27b \
  --python /opt/homebrew/bin/python3 --runs 3 --output /tmp/cognix-target.json
```

The evaluator approves only `main.py` diffs and the exact sandboxed unittest
argv in these disposable projects. It records each preview/decision, the full
conversation, tool calls/results, exit codes, elapsed time, model metadata and
source hashes. Product CLI sessions always ask for each write/command approval.

## Mandatory acceptance mapping

| Criterion | Actual method and evidence | Result |
| --- | --- | --- |
| AC1 — Native application | `cargo build --locked --release`; isolated `cargo install --locked --path . --root <temporary-directory>`; installed `cognix --help` from outside the checkout with PATH restricted to `/usr/bin:/bin`; `file` identifies Mach-O 64-bit arm64. | Pass on this Apple Silicon Mac. |
| AC2 — CLI | `tests/cli.rs`: project/model options, environment defaults, interactive/single task, activity, real approvals and EOF denial, invalid input, visible final prose, sanitized terminal output, SIGINT at input/model wait and during approved command execution, committed patch preservation. | Pass. |
| AC3 — Provider | `src/provider.rs`; `tests/provider.rs` exercises native Ollama payloads/probe, alternate model selection, missing server/model, cloud/unsupported tags, failed capability/probe, malformed/oversized/incomplete responses, timeout/cancellation and context errors. Both actual local models passed readiness/probe and fixture trials below. | Pass on recorded local tags. No hosted inference or automatic download. |
| AC4 — Agent/context | `tests/agent.rs`: ordered results and native/generated IDs, retained thinking/history across turns, duplicate/malformed batch rejection before effects, closed schemas and recoverable errors, final answers, step/context/provider stops, cancellation and queued unrun calls. | Pass. |
| AC5 — Tools/edits | `tests/runtime.rs`: all four tools, schema/type/path failures, missing/large/binary/nonregular files, bounded listings, unique-span/stale-hash checks, previews, approval/denial, atomic writes, concurrent edits/parent swaps and preservation of unrelated changes. | Pass. Existing-file targeted patches retain the baseline scope. |
| AC6 — Safety | Real macOS subprocesses in `tests/commands.rs` deny outside/symlink reads, root/outside writes, network, Git metadata/case aliases, forks and child spawning. Scratch writes work; environment is restricted to the documented set. Limits/cancel kill and reap the process. Directory-FD tools deny symlinks/hardlinks/traversal/Git aliases; `tests/root_changes.rs` rejects a replaced root. CLI tests send real SIGINT. | Pass on macOS Seatbelt. Linux is unsupported and fails closed before command approval/launch; real Linux validation is Not run and no Linux isolation is claimed. |
| AC7 — S1–S6 | Deterministic fixture test actually inspects, runs failing tests, approves/applies a correction and reruns to pass, asserting failure feedback and unchanged tests/user files. Actual local-model trials exercise the same flow with recorded approvals; alternate selection and unsupported models have deterministic tests. Setup, hardware, versions, exact tags/digests and outcomes are recorded here and in JSON. | Pass on both recorded models; observations are not a general success-rate guarantee. |
| AC8 — Truthful evidence | Runtime-owned ledger and separate model prose; `tests/agent.rs` and `tests/cli.rs` test unsupported success claims, all six outcome statuses and queued unrun work. `src/runtime_tests.rs` injects cancellation immediately before/after atomic commit, at spawn ownership, cleanup and completed-command return. Executed flags, exit codes/output, hashes, modified files and ledger survive. Real CLI SIGINT tests cover committed edits and running commands. | Pass. Completed work remains recorded; no automatic rollback or further unapproved action. |
| AC9 — Complete migration/docs | Cargo is the source of truth. Python package/entry points, old application regression tests and Python evaluation/docs scripts are retired after porting coverage. Rust check/evaluation examples replace the scripts. README, MVP, implementation and contributor/testing guidance describe Rust. Native build/install, formatting, lint, tests, docs and whitespace checks below ran successfully. | Pass. Historical Python evidence is explicitly separate. |

## Regression coverage migration

The Rust tests combine related cases instead of reproducing Python test-function
counts. The historical Python suite's assertions are represented as follows:

| Baseline coverage | Rust coverage |
| --- | --- |
| IDs/order, round trips, multi-turn context, generated/duplicate IDs, malformed envelopes and excessive batches | `tests/agent.rs`, `tests/provider.rs` |
| Tool schema errors and ordinary failure recovery, final answers, provider/step/context limits, pre/during-model cancellation | `tests/agent.rs` |
| Listing/read bounds, malformed arguments, missing/binary/large/FIFO files, stale/ambiguous patches, previews, denial and concurrent edits | `tests/runtime.rs` |
| Canonical root confinement, symlink and hardlink aliases, Git metadata/case aliases, parent/root swaps, user edits | `tests/runtime.rs`, `tests/root_changes.rs` |
| Exact command policy, actual exit/output/failure, scratch/environment isolation, outside reads/writes/network/Git denial, fork/spawn denial | `tests/commands.rs`, `tests/runtime.rs` |
| Time/output/closed-pipe limits, cancellation before launch and while running, process reaping | `tests/commands.rs` |
| Python signal-deferral ownership/restoration regressions around spawn, cleanup and rename | `src/runtime_tests.rs` injects the cancellation flag at these boundaries; Rust has no signal-driven stack unwinding or handler restoration. `tests/cli.rs` additionally sends actual SIGINT after patch commit and during command execution. |
| CLI task/root/model configuration, interactive approvals/EOF/invalid options, input/model interruption, sanitized visible prose and separate evidence | `tests/cli.rs` |
| HTTP native protocol, model readiness/capability/probe/cloud rejection, malformed/oversized/server/model/context errors, timeout/cancel | `tests/provider.rs` |
| S1–S6 fixture inspect/fail/approved patch/approved check/recovery and honest results | `tests/agent.rs` plus the two actual Rust trial records |

## Actual model trials

- **`qwen3-coder:30b`**: local Q4_K_M, 30.5B parameters, digest
  `06c1097efce0431c2045fe7b2e5108366e43bee1b4603a7aded8f21689e90bca`.
  [Rust raw record](rust-qwen3-coder-trials.json): **3 attempts, 3 successes,
  0 failures** on the final source. Every attempt inspected the fixture, observed
  an approved failing test, committed an approved `main.py` patch, and reran the
  approved check to pass. Elapsed seconds: 11.742, 4.751, 4.746, including startup
  probe (and model loading where needed).
- **`qwen3.8:27b`**: local Q4_K_M, 27.3B parameters, digest
  `aaee06c39dcf2437cde036998d960e1fc1494b8191be7cc9657d01e509097813`.
  [Rust target raw record](rust-qwen3.8-trials.json): **3 attempts, 3 successes,
  0 failures** on the final source. Every attempt inspected the fixture, observed
  an approved failing test, committed an approved `main.py` patch and reran the
  approved check to pass. Elapsed seconds: 41.988, 42.193, 42.463, including the
  startup probe.

Each successful trial requires final-answer termination, only `main.py` in the
modified-file ledger, a recorded failed check followed by a final passed check,
and byte-for-byte unchanged tests. Initial failures remain failed in the ledger;
model text does not change them. Two earlier development batches per tag also
observed 3/3 successes each (6/6 per tag). Final-source batches replace those
records rather than mixing revisions or counting earlier results in the
final-source totals. These six final observations do not establish a universal
success rate.

The installed native CLI also completed an actual local-model inspection task
from outside the checkout with its environment restricted to `PATH=/usr/bin:/bin`.
It read `main.py`, explained the zero-denominator behavior, left the fixture
unchanged and reported verification as not run (exit 0). The executable hash,
exact invocation and terminal output are in the [native CLI smoke record](rust-cli-smoke.json).

## Repository checks and remaining limitations

| Check | Actual result |
| --- | --- |
| `cargo build --locked --release` | Pass; native arm64 executable, initial optimized build 7.98 seconds on this host. |
| Isolated `cargo install --locked --path . --root <temporary-directory>`, installed `cognix --help`, `file` | Pass; executable runs outside checkout without a Python environment; Mach-O 64-bit arm64. Cached final install build 0.28 seconds. |
| Installed native CLI with actual Qwen3-Coder inspection task | Pass; exit 0, actual `read_file`, unchanged fixture, verification explicitly not run; raw smoke record linked above. |
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --locked --all-targets -- -D warnings` | Pass. |
| `cargo test --locked --all-targets` | Pass: 35 tests, no failures or skips on this Mac. Measured suite groups total about 2.3 seconds, excluding compilation. |
| `cargo run --locked --example check_docs` | Pass: 12 Markdown documents, 0 local-link/anchor/structure/template metadata errors. |
| `git diff --check` and staged equivalent | Pass. |
| Actual Rust/Ollama fixture trials | Final source: 3/3 per tag (Qwen3-Coder and Qwen3.8); both recorded failing-check recovery. |

An initial deterministic fixture-port failure used the wrong variable names and
was corrected. An initial HTTP fixture failure exposed macOS accepted sockets
inheriting nonblocking mode; the fixture now explicitly uses blocking accepted
sockets. The final suites above pass after those corrections. Rust time/output
bounds and cancellation preserve execution evidence without Python's exception
and handler-restoration race.

Seatbelt is a deprecated macOS facility; failure to launch it never retries
without isolation. Verification that needs child processes, dependency downloads
or project writes remains outside the documented policy. ACLs/xattrs are not
copied by atomic patch replacement, as in the baseline; ordinary source-file
mode bits and unrelated contents are preserved. A concurrent hostile process
running as the same OS user remains outside the file-tool isolation boundary.

Optional Linux sandbox validation and DeepSeek/comparative performance studies:
**Not run**; Linux commands fail closed. No CI run or deployment workflow exists;
local results do not imply CI/deployment success. These timings are feedback
measurements, not performance budgets. No implementation work remains in the
supported macOS scope; downstream must review the checkpoint and evidence
before integration.

Required post-merge verification: none required.
