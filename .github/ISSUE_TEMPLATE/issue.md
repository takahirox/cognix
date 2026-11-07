---
name: Issue
about: Report a problem or propose a focused change
title: ""
labels: ""
assignees: ""
---

<!-- Write the title and body in English. See:
https://github.com/takahirox/cognix/blob/main/docs/development-flow.md
-->

## Problem

Describe the problem and who or what it affects.

## Expected outcome

Describe the result that will resolve this Issue and the focused changes needed.
Identify scope boundaries and any known remaining work outside this Issue.

## Context and reproduction

Include relevant examples, logs, prior decisions and related Issues. For a bug,
give reproduction steps, expected results and actual results. Otherwise explain
why reproduction does not apply.

## Mandatory pre-merge acceptance

List concrete implementation requirements and checks verifiable before merge.
Assign stable identifiers (AC1, AC2, ...) and preserve them through revisions so
PR evidence can map to every criterion. Match checks to actual repository tooling.
Require human input, devices, private data or subjective approval only when this
Issue needs them; explain why and specify the expected evidence.

- [ ] **AC1:** Replace with a concrete, verifiable completion criterion.

## Validation already performed

Record each command or method, actual result and limitations, or state "Not run"
with a reason. Use **Pass** for an executed successful check, **Fail** for an
executed failed check, **Skipped** for a deliberately omitted check with a reason,
and **Not run** for an unperformed or unavailable check with its reason and resume
condition. Pending CI is **Not run**, with the pending run identified; it is not a
pass. Existing evidence does not replace validation of the eventual PR head.

## Optional validation

List useful additional checks separately from mandatory acceptance, or state
"None required."

## Required post-merge verification

List only genuinely required checks possible after merge, explaining why they
require merge and what evidence is needed, or state "None required." Keep them
pending until performed. A merge-triggered deployment cannot be a pre-merge gate.

See the [review and merge guidelines](https://github.com/takahirox/cognix/blob/main/docs/review-guidelines.md).
