---
id: journey_testing
title: Development journey testing
when_to_use: Verifying requested development API or browser journeys and independent Diagnostics package publication.
default_mode: discoverable
minimum_access: registered
---

# Development validation scope

Follow the user's requested validation scope. API-only tasks require authenticated V3 API checks and do not require browser journeys.

When a browser check is explicitly requested, resolve the exact route from `docs/ui-journeys.md` before opening the private Diagnostics browser. Use only the current authorized agent and conversation. Verify the final allowed route, bounded visible text and relevant API result. Read before every input action; never use arbitrary JavaScript or shell-driven authentication.

Screenshots are private bounded PNG artifacts with opaque IDs and authenticated API URLs. Capture only when useful or requested. Recording creates at most 30 PNG frames over 30 seconds and is not a video. Stop capture and close the browser on terminal paths.

No production origin, V2 JWT, localStorage authentication, database inspection or another caller's browser is part of this contract. Unknown input effects require inspection before retrying.

For independent package acceptance, compare plugin_revision, implementation_version and implementation_sha256 on actual tool results. An open browser remains pinned across compatible publication and rollback; a new profile adopts the current revision. Verify the registered visible checkpoint after rendering, not merely successful navigation.
