# Development flow

Work in cognix follows **Issue → implementation → PR → review → revision as needed
→ merge**. Add post-merge work only when actually required. Target `main` unless
the Issue or authorized workflow specifies another branch.

## Collaboration language

Use **English** for newly authored repository documentation, AI guidance,
templates, Issue/PR titles and bodies, reviews, and commit messages. The
maintainer's chat language and product localization are unaffected. Historical
material does not need translation solely to satisfy this convention.

## Scope and acceptance

Read the current Issue, relevant discussion and repository instructions. Use the
[Issue template](../.github/ISSUE_TEMPLATE/issue.md) to describe the problem,
expected outcome, context/reproduction and criteria with stable identifiers.
Resolve necessary ambiguity before dependent work and continue independent work.

Implement the smallest coherent change that fully satisfies the Issue. Check for
both omitted requirements and unnecessary additions. Avoid speculative
abstractions, unrelated refactors and unjustified frameworks or configuration.
Simplicity must preserve required behavior and validation.

Keep three categories distinct:

- **Mandatory pre-merge acceptance:** requirements and checks achievable and
  verifiable before merge.
- **Optional validation:** useful extra checks, not completion gates.
- **Required post-merge verification:** genuinely required checks possible only
  after merge, pending until performed. State "None required" when appropriate.

Do not invent completion gates, Issue-specific partial-only rules, or mandatory
human/device/private-data checks. Require human input or subjective approval only
when the actual Issue needs it, explaining why and what evidence is expected.
A merge-triggered deployment cannot be a pre-merge prerequisite. Separating these
categories does not waive implementation requirements or applicable checks.

## Implementation and validation

Inspect the current tree before editing. Apply the canonical
[UI/UX and layout principles](ui-ux-principles.md) when interfaces are introduced.
Choose checks using the [test and CI performance principles](testing-ci-principles.md)
and the tooling actually present. Cognix uses Python's standard library at runtime,
unittest for regression/integration tests, and Ruff for formatting/linting.
See the [README](../README.md#check-and-build) for actual commands and the
[validation record](validation.md) for measured results and remaining gaps.
There is no deployment workflow. Do not invent commands, runtimes or results.

For documentation changes, validate template metadata, Markdown structure,
relative links and heading anchors, cross-document consistency and whitespace.
Run `git diff --check`, or `git diff --cached --check` for staged changes. Inspect
the complete diff including new files; an unstaged diff alone omits untracked
files. Use available tooling and add no dependency solely for this baseline.
Unrelated runtime suites, deployed-site checks and human design approval are not
required for documentation-only work.

## PR, review and revision

Use the [PR template](../.github/pull_request_template.md) to explain the outcome,
focused changes, related Issues, remaining scope and validation actually performed.
Map evidence to every required Issue criterion at the current PR head. Report
Pass, Fail, Skipped and Not run accurately with reasons for gaps; pending CI is
not a pass. Keep optional validation and post-merge verification separate.

Use closing references only for complete Issue resolution. A partial PR must use
a non-closing reference, preserve unmet criteria and remaining work, and leave
the source Issue open. An explicitly scoped partial integration is permitted
under existing maintainer/workflow authorization; it does not establish full
Issue completion. Pending genuinely post-merge-only verification does not make
an otherwise complete implementation partial.

Follow the [review and merge guidelines](review-guidelines.md). Return missing
implementation, validation and other agent-fixable findings to the fix stage.
Review the updated head and relevant evidence after revision. Record genuine
external blockers separately and await pending CI rather than repeating unchanged
reviews, publications or merge attempts.

## Merge and follow-up

Merge only the reviewed current head after applicable mandatory checks pass,
under existing repository/workflow authorization and required protections. The
review guide defines readiness, including explicitly authorized partial scope;
these documents neither grant merge permission nor add a blanket human-approval
requirement.

Keep genuinely required post-merge-only verification pending until performed,
then record actual results and follow-up. Pre-merge success does not prove
deployment success. This documentation/template baseline needs no post-merge
verification.
