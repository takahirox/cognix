# Test and CI performance principles

Detect concrete defects at the lightest meaningful layer. Preserve necessary
assertions, regression confidence and integration coverage when moving or
simplifying checks.

## Current tooling and change-based validation

cognix currently contains documentation and templates. There are no application
tests, package/build commands, or executable CI/deployment workflows. No runtime
baseline or budget has been measured or established. Do not invent commands,
results, timings or executable CI. Product implementation, tooling and workflows
belong in separately scoped work.

Match checks to the actual change and repository tooling:

- Documentation/templates: validate metadata, Markdown structure, relative links
  and heading anchors, consistency, whitespace and the complete diff including
  new files. Run `git diff --check` or its staged equivalent. Add no dependency
  solely for this baseline and run no unrelated runtime suite.
- Logic, when introduced: prefer fast unit/component tests for calculations,
  decisions and data handling.
- Interfaces or integration boundaries, when introduced: check relevant behavior
  and accessibility; reserve costly integration/browser/device/service execution
  for boundaries that require those environments.
- Build/CI changes, when introduced: validate the affected pipeline and artifacts.

Separate fast feedback, required regression coverage, expensive exploratory
checks and deployment responsibilities, using appropriate triggers. Optional
exploration is not a completion gate. Record genuinely required post-merge-only
checks separately as pending until performed. This baseline requires none.

## Coverage and fixtures

Every check should catch an identifiable defect. Avoid tests that merely mirror
the implementation and equivalent duplicate checks. Select representative cases
for distinct paths and risky interactions; do not automatically take the
Cartesian product of formats, environments, inputs and states. Add combinations
when their interactions justify the coverage, preserving necessary regressions
and integration confidence.

Prefer small synthetic or appropriately licensed representative fixtures. Justify
large datasets, repeated downloads and external-resource dependencies by concrete
additional coverage that smaller local fixtures cannot provide. Record fixture
sources and applicable licenses.

## Reuse and scheduling

When executable tooling exists, reuse applicable builds and downloads with cache
keys reflecting their inputs, dependencies, toolchain and environment. Preserve
isolation between runs and prevent stale artifacts from passing as fresh results.
Avoid equivalent push/PR validation of the same revision. Cancel superseded work
when safe, preserving necessary deployment ordering.

Choose parallelism using measured elapsed time and resource use; extra workers
can increase setup, contention and cost. Keep independent work parallel only
where measurements justify it.

## Cost review and measured budgets

Review each new or expanded check for additional defect coverage, duplication,
runtime and maintenance cost. Investigate repeated setup, large downloads,
oversized fixtures and growth from multiplied combinations without weakening
necessary assertions or regression/integration confidence.

When executable tooling is introduced, establish repository-specific measured
baselines and budgets from representative runs, recording commands and
environments. Track elapsed feedback time, runner/resource use and queue time
separately. Investigate growth beyond those budgets and adjust layers, reuse,
selection or scheduling using evidence. Do not impose an invented universal time
limit or claim unmeasured performance improvements.
