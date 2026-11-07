# Cognix MVP v0.1

Cognix MVP v0.1 is a **local-first, single-agent CLI coding assistant capable of
autonomous, bounded edit-and-test loops**. It takes a development request for a
local project, lets the model choose tools, enforces user approval and runtime
limits, and reports actual results.

This specification resolves the documentation scope of
[Issue #4](https://github.com/takahirox/cognix/issues/4). The functional requirements
and scenarios below define engineering acceptance, not validation delivered by
the specification itself. See the [implementation](implementation.md) and
[current validation record](validation.md) for Issue #6 evidence and limitations.

The [long-term product vision (Issue #3)](https://github.com/takahirox/cognix/issues/3)
includes a shared engine for terminal and desktop experiences, richer context,
planning and persistent sessions. This MVP is the first bounded step toward that
vision; those broader capabilities are not release requirements here.

## Target user workflow

1. The user opens a local project in a terminal and starts Cognix, for example
   with `cognix --model qwen3.8:27b`.
2. The user asks: "Fix the division-by-zero bug in main.py and verify the fix."
3. The local model chooses tools to inspect relevant files and proposes a
   targeted patch. Cognix shows the proposed changes and asks for approval before
   modifying files or running commands.
4. Cognix applies the approved change, runs an approved verification command and
   returns the output, including any failure, to the model.
5. The model can attempt corrections within the configured limits, then reports
   changed files and the actual verification status. The user can interrupt at
   any point.

The startup command illustrates the interface; see the [README](../README.md)
for setup. The example model tag has not been verified as compatible.

## Functional scope

### F1 — CLI

Provide an interactive terminal session for one working directory. Show user
input, tool activity, final responses and actionable errors, with interrupt
support. A simple text CLI is sufficient; a rich TUI is deferred.

### F2 — Local model connection and selection

Use a locally running Ollama server as the initial provider. Accept an Ollama
model tag through a CLI option such as `--model <ollama-tag>`, with a configurable
default. CLI model selection is part of the MVP.

**Qwen3.8 27B is the first validation target, not a hard-coded dependency.** Its
availability and tool-calling compatibility must be verified during engineering
evaluation. DeepSeek or other models may be selected when compatible with the
Ollama tool-calling contract and local resource limits. Reject or clearly report
unsupported or missing tool-calling behavior instead of silently pretending that
a model can operate the agent.

Keep model access behind a narrow interface so providers such as llama.cpp or
LM Studio can be added later. Implementing additional providers is deferred. No
external AI API is required during normal operation after local setup is complete.

### F3 — Agent loop

Send the current conversation context and tool specifications to the model.
Validate each tool call, execute only authorized calls, append structured tool
outputs including errors, and resample until the model finishes or a stop
condition is reached. The model chooses the next action; the harness enforces
constraints.

Support cancellation, a bounded number of steps and clear errors. Stop on user
interruption, exhausted limits or an unrecoverable error, and report why the loop
stopped. A verification failure can inform another attempt within the same limits.

### F4 — Minimal tools

| Tool | MVP responsibility |
| --- | --- |
| `list_files` | Discover files within the project root. |
| `read_file` | Return bounded contents of a project file. |
| `apply_patch` | Apply a targeted, approved patch; avoid uncontrolled whole-file replacement. |
| `run_command` | Execute an approved verification command within the command policy. |

Validate arguments and return structured success/failure data for every tool,
including useful errors for invalid calls. Semantic indexing and Git write tools
are not required.

### F5 — Context

Preserve the user request, model responses, tool calls and tool results in the
current in-memory session. Respect available context limits. If they are exceeded,
fail transparently rather than silently dropping essential task information.
Persistent history, session resumption and sophisticated compaction are deferred.

### F6 — Safety

- Confine file reads and writes to the selected project root. Canonicalize paths
  and check symlinks so an apparently in-root path cannot escape the root.
- Show proposed changes before approval, prompt before file modifications and
  command execution, and preserve pre-existing user changes. Denied or
  unapproved actions must not execute.
- Treat model-generated commands as untrusted. Restrict execution to a defined
  safe/approved verification subset or an appropriately isolated environment;
  an approval prompt alone does not establish command safety. The exact subset
  or isolation mechanism remains a follow-up design choice.
- Enforce execution timeouts, bounded output and agent step limits, with a cancel
  path that stops ongoing work and prevents further actions. Concrete limit
  values remain to be specified and tested in engineering work.

### F7 — Honest outcome

Report changed files, verification commands actually run, their results and
unresolved failures. Distinguish passed, failed and unrun verification, including
work stopped by denial, cancellation or limits. Never claim tests passed when
they were not run.

## Acceptance scenarios

S1–S6 are engineering acceptance scenarios tracked by Issue #6. They are not
pre-merge gates for the historical documentation-only Issue #4. Actual results
and unverified targets are in the [validation record](validation.md).

- **S1 — Tool choice:** Given a small fixture project and a request to inspect a
  source file, a tool-capable local model invokes `list_files`/`read_file` and uses
  the actual returned content.
- **S2 — Edit and verify:** Given a reproducible division-by-zero bug in a fixture
  project, the agent proposes a patch, waits for approval, applies it, executes an
  approved verification step and accurately reports the outcome.
- **S3 — Recovery:** Given a deliberate failing verification result, the agent
  observes the failure and attempts a bounded correction or explains why it
  cannot proceed.
- **S4 — Safety:** Out-of-root paths, including symlink escapes, unapproved
  modifications/commands and canceled operations do not execute. Time and step
  limits are enforced, output is bounded and pre-existing user changes survive.
- **S5 — Model selection:** Exercise at least the initial Qwen3.8 27B Ollama setup.
  Demonstrate that CLI selection is not hard-coded to that tag and that
  unsupported models fail clearly.
- **S6 — Reproducibility:** Once implementation exists, document concrete
  setup/run/test commands and their real results, including the model tag,
  provider version, fixture and evaluation hardware needed to interpret them.

Initial evaluation hardware is an Apple M4 Max with 64 GB unified memory. For
probabilistic model behavior, record repeated-run results, including attempts,
successes and failures, rather than assuming 100% task success. Benchmark
thresholds can be established separately using actual hardware and fixtures; no
unmeasured performance or success-rate threshold is imposed here.

## Explicit exclusions

The following are outside MVP v0.1:

- Desktop GUI and rich TUI.
- Multiple agents and cloud inference/execution.
- Additional model providers and model switching mid-session.
- Persistent sessions and advanced context compaction.
- An advanced planning subsystem and semantic code indexing.
- MCP integration and extensive UI customization.
- Automatic Git commits and branch management.

Selecting a compatible model at startup through Ollama remains in scope; adding
another provider does not.

## Architectural proposals and unresolved choices

Prefer a shared **Agent Core + model-provider interface + tool runtime + CLI
adapter** boundary so a later desktop interface can reuse the core. This is an
architectural direction, not a fixed framework or a requirement to build desktop
infrastructure now. [OpenAI Codex](https://github.com/openai/codex) is a reference
for agent-loop, tool-routing and provider boundaries, without requiring feature
parity.

Rust is a candidate implementation language, not a settled requirement. Ollama is
the initial provider and Qwen3.8 27B the initial evaluation target; neither fixes
the implementation language or guarantees model compatibility.

The [implementation guide](implementation.md) records the selected language and
libraries, schemas, numeric limits and command policy. The original design called
for follow-up engineering to settle the implementation language and
libraries, provider/tool schemas and compatibility checks, default-model
configuration, context accounting, numeric limits, command policy or isolation,
and approval/cancellation behavior. Keep these decisions small and tied to
F1–F7 and S1–S6; no framework or speculative abstraction work is required for
this documentation task.

## Acceptance for this documentation task

Only the following gates resolve Issue #4. They do not require the future MVP to
run:

- **AC1:** Publish this English specification with the workflow, F1–F7, S1–S6
  and explicit exclusions.
- **AC2:** State that CLI model selection is in scope, Ollama is the initial
  provider, Qwen3.8 27B is a validation target and compatible DeepSeek/other
  models may be selected.
- **AC3:** Separate future implementation goals from documentation acceptance
  and label unresolved choices as proposals.
- **AC4:** Link the specification from the [README](../README.md), connect it
  to the long-term vision in Issue #3, and keep existing guidance consistent.
- **AC5:** Validate Markdown structure, links/anchors and diff whitespace using
  available repository tooling; report only validation actually performed.

Follow the [development flow](development-flow.md#implementation-and-validation),
[review guidelines](review-guidelines.md) and
[testing/CI principles](testing-ci-principles.md). Application setup, build and test
commands are in the [README](../README.md); runtime results are recorded separately
from this historical documentation acceptance.

Optional validation: review this specification against the first engineering
Issues when they are planned. This is not a pre-merge gate.

Required post-merge verification: none required.
