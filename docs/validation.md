# Issue #6 validation record

This record applies to the implementation files committed alongside it. No PR,
merge, Issue closure, or post-merge verification was performed. Qwen3.8-specific
compatibility remains unverified; do not use this checkpoint as evidence to close
Issue #6 without resolving the target-model acceptance gap.

## Environment and reproducibility

Evaluated on 2026-11-07, Apple M4 Max, 68,719,476,736 bytes (64 GiB) unified
memory, Darwin 27.2.0 arm64. Application/test interpreter: Python 3.13.13.
Verification interpreter: `/opt/homebrew/bin/python3` (Homebrew Python 3.15.2).
Ollama server/client: **0.34.4**. Real model: **`qwen3-coder:30b`**, local
Q4_K_M, 30.5B parameters, digest
`06c1097efce0431c2045fe7b2e5108366e43bee1b4603a7aded8f21689e90bca`.
No model was downloaded or substituted automatically. An existing Ollama install
was started locally for this evaluation.

The repository-owned synthetic fixture is `tests/fixtures/division`: a small
Python division function and two unittest cases. Zero denominators must return
`None`; the checked-in fixture intentionally fails that case. Tests copy it to
a disposable project. No external dataset is required.

## Acceptance mapping

| Criterion | Implementation and actual evidence | Status / limitations |
| --- | --- | --- |
| AC1 — CLI | `cli.py`, package entry point; `test_cli.py` exercises interactive task input, real approval prompt, visible tool activity/results, EOF, invalid options/root/task, and SIGINT at input/model wait. Installed executable smoke and wheel build below. | Pass on this Mac. |
| AC2 — Ollama/model selection | `provider.py`; local HTTP fixtures validate selected/default tag, payloads, server/model errors, unsupported tools/cloud tags, malformed/oversized responses, timeout/cancel and real tool behavior probe. Actual `qwen3-coder:30b` passed with Ollama 0.34.4. | Pass for available compatible model. **Not run** for Qwen3.8 compatibility: `/api/show` reports `model 'qwen3.8:27b' not found`. Target-specific acceptance remains pending. |
| AC3 — Loop/context | `agent.py`, `test_agent.py`: call IDs and ordering, full result round trip, multi-turn history, normal stop, malformed batch, schema errors, duplicate IDs, tool failure, step/context/provider stops and cancellation. | Pass. |
| AC4 — Tools | `runtime.py`, `commands.py`; `test_runtime.py` and fixture integration exercise all four tools, preview-before-write, argument failures, missing/binary/large files, stale hashes and ambiguous patches. | Pass; targeted existing-file edits only, as documented. |
| AC5 — Safety | Directory-FD path operations; exact argv policy plus OS sandbox. Real subprocess tests deny outside reads, symlink escapes, root/outside writes, network, Git metadata, inherited secrets, forks/spawns; enforce time/output limits and cancellation. Fixtures exercise denial, queued unrun calls, parent swaps and preservation of existing edits. | Pass on macOS Seatbelt. Linux backend **Not run** on a real Linux host; no Linux guarantee claimed. |
| AC6 — Edit/verify/recovery | Deterministic provider runs actual sandboxed failing tests, reads/patches with approvals and reruns to pass; assertions check failure returned to model and unrelated changes preserved. Three separate actual-model trials recorded below. | Pass on available model. Qwen3.8-specific trials **Not run**. |
| AC7 — Evidence reporting | `Agent.report`, `cli.render`, `test_cli.py`: passed/failed/not_run/denied/canceled/limit_reached; invalid args; model's false “tests passed” prose is not rendered as evidence. | Pass. |
| AC8 — Reproducible checks/docs | README, implementation guide, this record, executable doc checker, package build, Ruff and unittest checks below. Raw provider/approval/tool results in `ollama-trials.json`. | Pass for recorded environment; target-model gap remains explicit. |

## Actual-model trials

Run from repository root:

```sh
.venv/bin/python scripts/evaluate_ollama.py --model qwen3-coder:30b \
  --python /opt/homebrew/bin/python3 --runs 3 --output docs/ollama-trials.json
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

**Pass (availability check):** `POST /api/show` with
`{"model":"qwen3.8:27b"}` returned `{"error":"model 'qwen3.8:27b' not found"}`.
`GET /api/tags` did not list that tag. No Qwen3.8 compatibility or success result
is claimed. Resume that validation when the exact local tag is available and
its actual tool probe and repeated fixture trials can be recorded. Compatible
DeepSeek/second-model comparison and Codex comparisons were **Skipped**, optional.

## Repository checks

| Check | Result |
| --- | --- |
| `python3 -m venv .venv` and `.venv/bin/python -m pip install '.[dev]'` | Pass: package built/installed with Ruff 0.17.4; no runtime dependencies. |
| `.venv/bin/python -m unittest discover -s tests -v` | Pass: 39 tests, no failures/errors/skips, 12.696 seconds on this host. Covers real sandbox execution and HTTP/CLI boundaries without inference. |
| `.venv/bin/ruff check .` | Pass. |
| `.venv/bin/ruff format --check .` | Pass. |
| `.venv/bin/python scripts/check_docs.py` | Pass: 11 Markdown documents, 0 local-link/anchor/structure/template-metadata errors. |
| `.venv/bin/python -m pip wheel --no-deps --wheel-dir dist .` | Pass: built `cognix-0.1.0-py3-none-any.whl`. |
| Fresh temporary venv, install wheel with `pip install --no-deps`, run installed `cognix --help` from outside checkout | Pass: imports resolved inside the clean environment, entry point exited 0. |
| `git diff --check` and staged equivalent | Pass. |

The final three model trials took 4.441, 4.232 and 4.237 seconds respectively
(including each startup probe). The test-suite time is a first observed local
feedback baseline, not a performance budget or cross-machine guarantee. The
trials above were repeated after the final macOS child-process restriction;
earlier exploratory runs are not substituted for this evidence. Style cleanup
and sandbox-policy findings were corrected before these final checks.

Required post-merge verification: none required. No CI run exists for this
checkpoint; local results do not imply CI or deployment success.
