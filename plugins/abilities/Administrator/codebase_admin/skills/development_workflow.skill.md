---
id: development_workflow
title: V3 development workflow rules
when_to_use: Use when making any WebAgent v3 source, UI, configuration or release change, to apply the project's standing rules.
default_mode: discoverable
examples: start a v3 feature; where should this new code live; release and commit a change
keywords: workflow, modules, release, commit, mobile
minimum_access: registered
---

# V3 development workflow rules

Standing rules from the project owner. They complement Codebase Admin and the
engineering_lessons skill and grant no extra authority.

## Scope and compatibility

- V3 is a development environment with no existing users needing migration.
  Unless asked, implement the intended current model directly: no migration
  machinery or preservation of obsolete schemas, settings, prompt formats or
  permission assignments. This never permits deleting runtime data, overwriting
  another writer's work, broadening permissions or bypassing isolation,
  authorization, approvals or broker safeguards.
- V2 is read-only reference (public repository `botboss3000/webagent-dev2`).
  Production needs a separate explicit request.
- Completed development changes are deployed by default; pause to verify first
  only when the user asks for verify-first.

## Where code lives

- Priorities: app performance and security first, then broker build speed.
  Never trade runtime performance or a security check for a narrower build.
- Put new and changed code in its owning module crate (`src/` for pure rules and
  their tests, `host/` for root adapters), not root `src/`. Do not grow root
  `module:general` code where an owning module exists. When a change touches
  root code that clearly belongs to a module, ask whether to move it now or edit
  in place.
- Keep domain behaviour tests with the owning module; root/host tests cover
  wiring, admission, authorization, error mapping and delegation only.
- Reservations follow edited files and their owners: extracted modules reserve
  `module:backend:<name>`, unsplit root code `module:general`, shared build files
  and unknown paths `module:workspace`. Rust compiles serialize through the
  `execution:rust-build` token, which never blocks edits.
- Every Rust edit brings the files it touches to the code standard in the
  rust_quality skill; an extraction brings its whole module. Do not widen
  reservations for cleanup.

## Configuration

Whenever a change creates or modifies configuration (a live JSON file in
`live-files.json`, a key in one, an agent or per-user setting), load the
configuration_settings skill and ship the setting descriptor in the same patch.
Keep one live config file next to its code; no seeded copies. Move hard-coded
tunables into JSON config.

## UI

- Every UI change works at phone width (360–412 CSS px) and desktop with one
  shared layout. Put controls and help text below their heading, let rows wrap,
  keep text and buttons inside their container. Check both widths before
  release and report both.
- Whoever changed the UI validates it in a browser, navigating directly to the
  registered route.
- Apply the ui_controls defaults to every button and button-like control.

## Driving the app

Use the HTTP API for chat messages, delegation, replies, run progress, settings
and state. Use a browser journey only for rendering, layout, interaction and
visual checks. The login cookie is Secure: an HTTP client on the Tailscale origin
must send the Cookie header explicitly, and browser-authenticated writes need
the CSRF header. Never print credentials, cookies or tokens.

## Release close-out

After a successful release, verify healthy containers, readiness 200 and source
promotion, then commit exactly the release's paths with a message naming the
change and broker job/release ID, and release claims with that commit. Do not
hold the commit for acceptance checks; run those afterwards. Inspection
artifacts go in a subdirectory of `build-jobs` (for example
`coordination/checks/<task>`), never top-level `build-jobs/*.json`.
