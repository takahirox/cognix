# Historical Python validation (Issue #6)

This is historical evidence for the retired Python implementation, not Rust
validation. The source and executable evaluator remain available in Git history
before the Issue #8 migration. Current evidence is in [validation.md](validation.md).

# Issue #6 validation record

This record applies to the implementation files committed alongside it. The
SIGINT fixes and target-model evidence still require review at the published
revision. The draft PR uses a non-closing `Refs #6` reference. No push, merge,
Issue closure, or post-merge verification was performed in this fix stage.

## Environment and reproducibility

Evaluated on 2026-11-07, Apple M4 Max, 68,719,476,736 bytes (64 GiB) unified
memory, Darwin 27.2.0 arm64. Application/test interpreter: Python 3.13.13.
Verification interpreter: `/opt/homebrew/bin/python3` (Homebrew Python 3.15.2).
Ollama server/client: **0.34.4**. Real model: **`qwen3-coder:30b`**, local
Q4_K_M, 30.5B parameters, digest
`06c1097efce0431c2045fe7b2e5108366e43bee1b4603a7aded8f21689e90bca`.
The evaluation target **`qwen3.8:27b`** was explicitly installed using
`ollama pull qwen3.8:27b` (exit 0): local Q4_K_M, 27.3B parameters, digest
`aaee06c39dcf2437cde036998d960e1fc1494b8191be7cc9657d01e509097813`.
An existing Ollama install was started locally for this evaluation. Normal
application startup does not download models automatically.

The repository-owned synthetic fixture is `tests/fixtures/division`: a small
Python division function and two unittest cases. Zero denominators must return
`None`; the checked-in fixture intentionally fails that case. Tests copy it to
a disposable project. No external dataset is required.

## Acceptance mapping

| Criterion | Implementation and actual evidence | Status / limitations |
| --- | --- | --- |
| AC1 — CLI | `cli.py`, package entry point; `test_cli.py` exercises interactive task input, real approval prompt, visible tool activity/results, EOF, invalid options/root/task, and SIGINT at input/model wait. Installed executable smoke and wheel build below. | Pass on this Mac. |
| AC2 — Ollama/model selection | `provider.py`; local HTTP fixtures validate selected/default tag, payloads, server/model errors, unsupported tools/cloud tags, malformed/oversized responses, timeout/cancel and real tool behavior probe. Actual `qwen3-coder:30b` and `qwen3.8:27b` passed the startup behavior probe with Ollama 0.34.4. | Pass on both recorded local tags, including the target-model setup. |
| AC3 — Loop/context | `agent.py`, `test_agent.py`: call IDs and ordering, full result round trip, multi-turn history, normal stop, malformed batch, schema errors, duplicate IDs, tool failure, step/context/provider stops and cancellation. | Pass. |
| AC4 — Tools | `runtime.py`, `commands.py`; `test_runtime.py` and fixture integration exercise all four tools, preview-before-write, argument failures, missing/binary/large files, stale hashes and ambiguous patches. | Pass; targeted existing-file edits only, as documented. |
| AC5 — Safety | Directory-FD path operations; exact argv policy plus OS sandbox. SIGINT regressions cover launch before ownership, repeated interruption during cleanup, and patch interruption before/after commit with truthful execution and modified-file evidence. Real subprocess tests deny outside reads, symlink escapes, root/outside writes, network, Git metadata, inherited secrets, forks/spawns; enforce time/output limits and cancellation. Fixtures exercise denial, queued unrun calls, parent swaps and preservation of existing edits. | Pass on macOS Seatbelt. Linux backend **Not run** on a real Linux host; no Linux guarantee claimed. |
| AC6 — Edit/verify/recovery | Deterministic provider runs actual sandboxed failing tests, reads/patches with approvals and reruns to pass; assertions check failure returned to model and unrelated changes preserved. Three actual-model trials per tag are recorded below; every Qwen3.8 trial observed a failed check, patched main.py and reran to pass. | Pass on both recorded models; observed counts do not establish general success rates. |
| AC7 — Evidence reporting | `Agent.report`, `cli.render`, `test_cli.py`: passed/failed/not_run/denied/canceled/limit_reached; invalid args; SIGINT launch/commit regressions preserve executed and modified-file evidence; model's false “tests passed” prose is not rendered as evidence. | Pass. |
| AC8 — Reproducible checks/docs | README, implementation guide, this record, executable doc checker, package build, Ruff and unittest checks below. Raw provider/approval/tool results in `ollama-trials.json` and `ollama-qwen3.8-trials.json`; target record includes installed metadata and implementation/fixture source hashes. | Pass for recorded environment; Linux limitation remains explicit. |

## Actual-model trials

Run from repository root:

```sh
.venv/bin/python scripts/evaluate_ollama.py --model qwen3-coder:30b \
  --python /opt/homebrew/bin/python3 --runs 3 --output docs/ollama-trials.json
ollama pull qwen3.8:27b
.venv/bin/python scripts/evaluate_ollama.py --model qwen3.8:27b \
  --python /opt/homebrew/bin/python3 --runs 3 --output docs/ollama-qwen3.8-trials.json
```

The [raw trial record](ollama-trials.json) includes the exact task, provider
messages, tool results, commands, approval previews/decisions, hardware, provider
version, elapsed time and success/failure counts. The harness automatically
approves only `main.py` diffs and its exact sandboxed fixture check, within
disposable directories. Normal CLI sessions still prompt for each action.

**Pass:** 3 attempts, 3 observed successes, 0 failures on `qwen3-coder:30b`.
Each attempt inspected fixture files, applied an approved `main.py` correction,
executed an approved sandboxed unittest command and received exit code 0 with
two tests passing. These three observations do not establish a general 100%
success rate. Failed-check recovery is deterministic regression evidence,
separate from these probabilistic trials.

The earlier missing-model result reflected local setup. The target is in the
[official Ollama catalog](https://ollama.com/library/qwen3.8:27b); after the explicit
pull, `/api/show` and `/api/tags` returned the installed tag, with `completion`,
`tools`, `vision` and `thinking` capabilities. The actual Cognix startup probe
passed before each of the three target trials; capabilities alone were not
used as compatibility evidence.

**Pass:** 3 attempts, 3 observed successes, 0 failures on `qwen3.8:27b`, recorded
in the [target trial record](ollama-qwen3.8-trials.json). Each attempt read the
fixture source and tests, ran an approved check that failed with the intentional
zero-denominator error, applied an approved `main.py` patch, and reran the approved
sandboxed check to pass (exit 0, two tests). Tests were preserved byte for byte,
and `main.py` was the sole modified file. The initial failed checks remain
recorded as failed; the final model answer is separate from that evidence.
These three observed successes do not establish a general 100% success rate.

The target record also contains the exact installed model metadata, download
command/result, and SHA-256 hashes of the implementation, evaluator and fixture
files used. Both trial records include actual approval previews, decisions,
provider messages and results. Optional DeepSeek and Codex comparisons were
**Skipped**; Linux isolation remains **Not run** on a real Linux host.

## Repository checks

| Check | Result |
| --- | --- |
| `python3 -m venv .venv` and `.venv/bin/python -m pip install '.[dev]'` | Pass: package built/installed with Ruff 0.17.4; no runtime dependencies. |
| `.venv/bin/python -m unittest discover -s tests -v` | Pass: 46 tests, no failures/errors/skips, 13.376 seconds on this host. Covers real sandbox execution and HTTP/CLI boundaries without inference. |
| `.venv/bin/ruff check .` | Pass. |
| `.venv/bin/ruff format --check .` | Pass. |
| `.venv/bin/python scripts/check_docs.py` | Pass: 11 Markdown documents, 0 local-link/anchor/structure/template-metadata errors. |
| `.venv/bin/python -m pip wheel --no-deps --wheel-dir dist .` | Pass: built `cognix-0.1.0-py3-none-any.whl`. |
| Fresh temporary venv, install wheel with `pip install --no-deps`, run installed `cognix --help` from outside checkout | Pass: imports resolved inside the clean environment, entry point exited 0. |
| `git diff --check` and staged equivalent | Pass. |

The refreshed `qwen3-coder:30b` trials took 4.027, 4.035 and 4.156 seconds;
`qwen3.8:27b` trials took 43.070, 35.863 and 35.265 seconds (including each
startup probe). These observations and the test-suite time are local feedback
measurements, not performance budgets or cross-machine guarantees. The SIGINT
regressions were checked on the final implementation; both models' fixture
results remain separate from deterministic security and cancellation tests.

The handler-restoration fix extends SIGINT deferral through tool return, cleanup
and ledger insertion. Two added regressions inject SIGINT immediately after the
normal handler is restored: committed patches retain their digest and modified
file evidence; completed commands retain `executed=true`, exit code 0 and output.
Both regressions fail against the preceding implementation and pass with this
fix. Ruff lint/format, documentation and whitespace checks also passed.
Actual-model trials were **Not run** again for this cancellation fix; their
stored source hashes identify the earlier trial revision. Real Linux/bubblewrap
validation remains **Not run**, pending the review question's external evidence.

Required post-merge verification: none required. No CI run exists for this
checkpoint; local results do not imply CI or deployment success.
