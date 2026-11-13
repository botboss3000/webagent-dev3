# Codebase Admin

Develop WebAgent v3 itself. This guidance is general; each tool's own skill
carries its actions, parameters and limits, and `list_tools` shows what is
available. Repository instructions, tool schemas and live policy stay
authoritative; loading guidance never grants a tool or deployment.

## Source and release model

- Source lives in `/code`. Application changes go through one isolated broker
  task per independent outcome: edit the task's draft, check it, then finish.
  Editing a task never deploys; only an explicit finish builds, tests, deploys
  and promotes, and only when the user's request authorises release.
- Live files (the `.json` and `.md` files listed in `/code/live-files.json`) are
  read by the running app and are edited in place; any other direct checkout
  edit changes nothing and shows up as drift.
- Broker ID names one work item, Broker Job ID one execution attempt, Broker
  Release ID one publication. Continue the current work item for related edits;
  start a new one for an unrelated outcome. Identity is supplied automatically:
  never look up, guess or copy a session ID.
- Builds run in the background and report by notice. Prove a change with checks
  on the same unchanged input digest before finishing; a full build is slow and
  fails late. A completed release commits its exact paths and closes itself.
- After any uncertain effect, inspect the existing receipt and job; never
  repeat an uncertain finish, deployment or external write.
- File tools and commands reach only this session's workspace and the
  checkout's source; use the app's APIs for app data, never its databases.

## Standing rules

- V3 is a development environment with no users needing migration: implement
  the intended current model directly, with no migration machinery or preserved
  obsolete schemas, unless asked. This never permits deleting runtime data,
  overwriting another writer's work or weakening isolation, authorization or
  approvals. V2 is read-only reference; production needs a separate request.
- App performance and security come first, then build speed. Never trade a
  security check for a narrower build.
- New and changed code goes in its owning module crate (`src/` for pure rules
  and their tests, `host/` for root adapters), not root `src/`. Domain tests
  stay with their module; root tests cover wiring only. When a change touches
  root code that clearly belongs to a module, ask whether to move it now.
- Preserve other writers' changes and all runtime data.

## Smallest complete change

Solve the whole problem with the least new code. Before writing, list every
place the change must reach (callers, tests, fixtures, config). Reuse what the
codebase already has; add no unrequested option, abstraction or dependency;
finish every caller and test the change breaks; fix a bug's root cause once in
shared code. Never cut validation at trust boundaries, error handling that
prevents data loss, security or accessibility. End with what you skipped or did
not check.

Leave no dead code. When a fix turns out not to work, remove what it added (code,
tests, styles, config) in the same task before or alongside the next attempt,
unless it is still needed; keep only what the working fix uses. Remove code your
change made unused, and do not leave commented-out code or unused helpers behind.

## Evidence

- Report only what evidence shows; an absent counter is unavailable, not zero.
- Separate fixture failures (timing, stale stubs, hidden controls) from product
  failures; fix the fixture rather than forcing a hidden control.
- Review a route end to end (registration and dispatch together) and verify
  activation separately from deployment. A user-facing route must work for a
  non-admin caller.
- A check verifies a fix only if it first reproduces the user's failure. When it
  cannot (an emulator, fixture or browser that never shows the bug), the fix is
  unverified: say so, do not mark the work complete, and ask the user to confirm
  on their device. When the user reports a fix still fails, find what your checks
  miss on their device instead of repeating the checks that passed.

## Scope and efficiency

Answer the scope asked: once the requested files, facts or change are done,
report and offer to go further instead of tracing adjacent layers unasked.
Batch independent searches and reads in one step.

## Specialised guidance

| Task | Load skill |
| --- | --- |
| Review, test assessment, diagnosis, release readiness | `code_verification` |
| Rust code | `rust_quality` |
| HTTP routes, storage lookups, request input, cross-user features | `security_practices` |
| Any configuration value or setting | `configuration_settings` |
| Abilities, guidance, JSON, UI assets, Python or other non-Rust files: choosing a release lane | `live_plugin_publication` |
| Any UI change: layout, controls, theming, option lists | `ui_design` |
| The user wants UI options or variants to choose from | `ui_proposals` |
| Designing in tool policy, file access, egress, browser or storage limits | `roadmap` |
