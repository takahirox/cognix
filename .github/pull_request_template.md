<!-- Write the title and body in English. See:
https://github.com/takahirox/cognix/blob/main/docs/development-flow.md
-->

## Problem and outcome

Link the source Issue and explain the problem, expected outcome and resulting
behavior. Include context or reproduction and a before/after example when useful.

## Focused changes

Describe what changed and why each change is needed for the stated outcome.

## Related Issues and remaining scope

Use `Closes #<number>` only for a complete Issue resolution. A partial PR must use
`Related to #<number>`, list remaining implementation and validation by criterion,
and leave the source Issue open. Record the existing maintainer/workflow
authorization for an explicitly scoped partial integration. State "None" when no
scope remains. Genuinely post-merge-only checks may remain pending for an otherwise
complete implementation.

## Mandatory pre-merge acceptance and validation

Identify the current PR head commit assessed below. Add a row for **every** required
Issue criterion, including criteria outside a partial PR's declared scope. Use its
stable identifier or quote the requirement if no identifier exists. Record the
implementation location, actual check method or command, result, and limitations.

PR head: <!-- commit SHA -->

| Issue criterion | Implementation or document location | Check and evidence | Status and reason |
| --- | --- | --- | --- |
| AC1 | Relevant location | Actual method and result | Pass / Fail / Skipped / Not run; explain gaps |

Use **Pass** only for checks actually executed successfully, **Fail** for executed
checks that failed, **Skipped** for deliberately omitted checks with reasons, and
**Not run** for unperformed or unavailable checks with their reasons or blockers
and resume conditions. Report pending CI as **Not run** with its run reference and
pending state; never as Pass. A skipped or missing mandatory check does not meet
acceptance. Do not claim full Issue completion while required pre-merge criteria
remain unmet. Follow the [validation guidance](https://github.com/takahirox/cognix/blob/main/docs/development-flow.md#implementation-and-validation).

## Optional validation

Report additional checks using the same statuses and evidence, or state
"None required."

## Required post-merge verification

List only genuinely required checks possible after merge, why they require merge
and the expected evidence, or state "None required." Keep these **Pending** until
performed, then record actual statuses, evidence and follow-up work. Pre-merge
results do not establish deployment or publication success.
