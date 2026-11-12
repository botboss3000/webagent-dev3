---
id: minimal_change
title: Smallest complete change
when_to_use: Use on any coding task (writing, fixing, refactoring, reviewing, choosing dependencies) to keep the change minimal and complete.
default_mode: discoverable
examples: implement this simply; fix this bug; is this over-engineered
keywords: minimal, simple, yagni, scope
minimum_access: registered
---

# Smallest complete change

Solve the whole problem with the least new code, and reply so a busy person
understands in one read. End with one or two lines: what you skipped or did not
check, and any risk the user must know.

## Before you write

Read the task and the code it touches. List every place the change must reach:
callers, tests, fixtures, config, exports. Check what it could break for users:
data destroyed or exposed, callers that stop working. That is scope; extra
features are not.

## Take the first option that fully works

1. Does it need to exist? Skip unrequested features, options and flexibility;
   name them in one line.
2. Already in this codebase? Use it the way surrounding code does.
3. Standard library or platform feature (unless the project has its own)?
4. An installed dependency? Never add one for a few lines.
5. One line a reader gets at a glance?
6. Otherwise the minimum code that works.

- Be lazy about the solution, never about the change: finish callers, tests and
  fixtures your change breaks.
- No abstraction, wrapper, option, config or "for later" code nobody asked for.
  Deletion beats addition. Keep the existing structure and conventions.
- Comment only the why the code cannot show.
- Bug fix: find every caller first, then fix the root cause once in shared code.
- Moved or merged code keeps its error handling and validation.
- New non-trivial logic leaves one small test or self-check.
- A shortcut with a known limit gets `shortcut: <the limit>, <when to upgrade>`.

Never cut validation at trust boundaries, error handling that prevents data
loss, security, accessibility, or anything the user asked for.

This guidance never relaxes the broker workflow, authorization, isolation,
verification, deployment or commit requirements.
