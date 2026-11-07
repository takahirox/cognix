# Review and merge guidelines

Follow the [development flow](development-flow.md), including the
[English collaboration convention](development-flow.md#collaboration-language).

## Review the current head and full Issue

Read the current Issue and discussion, PR description, complete diff including new
files, and validation evidence. Identify the actual current PR head commit. Assess
every required Issue criterion against that head and map it to implementation and
evidence, recording any gap. Evidence from an earlier head must still apply to the
current changes; rerun affected checks when needed. A changed head needs review of
the changes and relevant evidence before merge.

Check correctness, consistency and affected documentation. Look for missing
requirements as well as unnecessary additions: speculative abstractions,
unrelated refactors, unjustified frameworks or configuration. Prefer the smallest
coherent change that fully satisfies the Issue. Review applicable interfaces
against the canonical [UI/UX and layout principles](ui-ux-principles.md).

## Evaluate evidence

Use the [validation guidance](development-flow.md#implementation-and-validation)
and [test and CI performance principles](testing-ci-principles.md). Check that
validation fits the change and actual tooling. For each new check, assess its
additional defect coverage, duplication, runtime and maintenance cost while
preserving required assertions, regression confidence and integration coverage.
Documentation checks cover metadata, Markdown structure, links/anchors,
consistency and whitespace without unrelated runtime suites or dependencies.

Verify actual Pass, Fail, Skipped and Not run results and reasons for gaps. Pending
CI is not a pass. Skipped or unperformed mandatory checks remain unmet. Keep
optional validation separate from mandatory pre-merge acceptance. Do not invent
completion gates or Issue-specific partial-only rules. Human input, devices,
private data or subjective approval are needed only when the actual Issue
requires them, with a reason and expected evidence.

## Findings, blockers and waiting

- **Agent-fixable finding:** state the affected criterion or location, concrete
  defect or missing implementation/validation, and expected correction. Return it
  to the fix stage; do not silently redefine incomplete work as partial success.
- **External blocker:** state the concrete missing evidence, dependency or
  decision, why the agent cannot resolve it, and the resume condition. Continue
  independent work and keep agent-fixable defects separate.
- **Pending CI:** await the existing run instead of repeating unchanged reviews,
  publications or merge attempts. On failure, inspect logs and classify the
  actual cause (implementation defect, infrastructure problem, or external
  dependency) before fixing or retrying. Retry only when the cause justifies it.

Preserve progress, findings, evidence and resume conditions while blocked. A
completed check result, changed code or resolved blocker can justify another
review; unchanged state cannot. Do not repeatedly request an already-resolved
decision or treat silence as agreement. Apply owner decisions only within their
explicitly agreed scope.

## Readiness and merge

Full Issue completion requires all implementation and mandatory pre-merge
criteria to be met, with no unresolved blocking findings. An explicitly scoped
partial integration may proceed under existing maintainer/workflow authorization
when its declared scope is complete and its applicable mandatory checks pass.
Still assess every Issue criterion, preserve unmet criteria and remaining work,
use a non-closing reference, and keep the source Issue open. Partial integration
does not establish full Issue completion. Use closing references only for a
complete resolution.

Merge only the reviewed current head when applicable mandatory checks pass and
the readiness conditions for the authorized scope hold. Follow existing
repository/workflow authorization and required protections. Do not bypass checks
or introduce a blanket new human-approval requirement. If the head changes,
reassess affected review and validation before proceeding.

## Post-merge verification

Only genuinely required post-merge-only checks belong here. A merge-triggered
deployment cannot be a pre-merge prerequisite, and pre-merge results do not prove
deployment or publication success. Keep these checks pending until performed,
then record actual results and follow-up. They do not waive required pre-merge
checks or implementation. State "None required" when appropriate; this
documentation/template baseline requires none.
