---
id: security_practices
title: Security practices
when_to_use: Use when adding or changing HTTP routes, storage lookups, shared or cross-user features, tool execution, UI rendering, or anything that handles request input.
default_mode: discoverable
minimum_access: registered
---

# WebAgent v3 security practices

These rules come from the 2026 security audit of v3. Apply them to every change. A new route or handler that breaks one is a defect, even if its tests pass.

## Authorization: who may touch this resource

- **The server decides identity.** Take the caller only from the auth middleware's identity (`_user_id`, `_role`) or a server-sealed token. Never from a body field, query, or header such as `Origin` or `X-Forwarded-For`.
- **Look up through the caller's own store.** `auth` swaps `app.store` for `app.store.for_user(user_id)`, which opens that user's own database. An ID lookup there can only see the caller's rows; this is v3's equivalent of row-level security. Keep it that way:
  - Never open another user's store (`for_user(other)`) except through an existing membership gate (`shared_chat::scope`/`resolve_store`, `run_app`).
  - Never read user data from the base, unscoped store in a handler.
- **A second ID must belong to the path resource.** When a route names a session and another ID (`/sessions/{sid}/attachments/{aid}`), bind the second ID to `sid`. Either key the record by `sid` (`memory:{sid}:{id}`, `record_key(sid, id)`), or compare `record.session_id == sid` after the lookup.
  - This matters most in shared chats, where a participant's requests run against the owner's whole database.
  - Return the same 404 for "doesn't exist" and "not yours", so IDs can't be probed.
- **Agent routes check the agent.** Every `/agents/{id}/...` handler checks the caller with `agent_authority(...).can_manage` for writes, or `live_access(...).admitted()` for member reads. Never trust a client-sent agent_id or session_id without that check.
- **Widening access is owner-only.** Anything that lets more people see a conversation (invitations, sharing settings) requires `SessionAccess::is_owner()`. Revoking or narrowing access may be open to participants.
- **The UI gate is not the gate.** Hiding a button is cosmetic. The handler must enforce the same rule, and the UI should read its flags (`can_invite`, `can_send`, …) from the server.
- **Public routes bind the caller another way.** A route outside `auth` must identify its caller through a one-time code, a digest-keyed secret, a sealed token, or PKCE. Re-check the owner's live access when the token is used. One-time setup routes (such as setup-admin) must refuse once done.

## Tests that prove it

- For every access rule, test both sides with two real accounts (see `active_user` and `cookie` in `crates/modules/chat/host/api/shared_chat.rs` tests): the allowed caller succeeds, and the other user gets 403 or 404.
- Keep these tests with the module that owns the rule.

## Injection: SQL and key scans

- **Values go through `.bind()`.** Only compile-time constants (table or column names from a fixed list) may be interpolated into SQL with `format!`. Never request, model, tool or ID text.
- **Prefix scans must not treat IDs as patterns.** `LIKE '{prefix}%'` makes `_` and `%` inside an ID act as wildcards, and `shared_default` already contains `_`. Use a key range (`key >= ? AND key < ?`) or escape the prefix with `ESCAPE '\'`.

## Rendering untrusted text in the UI

- **Treat everything that isn't a constant as untrusted.** That covers model output, tool results, file contents, filenames, agent and ability names, error messages and wiki text.
- **Markdown always goes through DOMPurify.** Use `DOMPurify.sanitize(marked.parse(text), {FORBID_ATTR:['style']})`, as `ui/chat/core/message-view.js` does. Never `marked.parse` straight into `innerHTML`, because marked keeps raw HTML.
- **Plain text uses `textContent`.** Error messages included.
- **Escape helpers used inside attributes must escape `"` and `'`** as well as `& < >`. The `textContent`→`innerHTML` trick does not escape quotes.
- **The CSP limits the damage, not the bug.** It blocks inline script, but injected HTML can still draw a fake approval card or overlay (`style` is allowed).

## Tool execution and outbound requests

- **File tools stay inside what the session may touch.** That is its workspace, plus the checkout for Codebase Admin. Never `/state` (encryption key, provider tokens, every user's databases) or another user's `/workspaces/<id>`. An absolute path must still be checked against the allowed roots after resolution, using O_NOFOLLOW walks rather than check-then-open.
- **No elevated capabilities for model-run code.** Sandboxed shell and python children must not inherit ambient capabilities such as `CAP_DAC_READ_SEARCH`, and their environment stays scrubbed.
- **Run commands as argv lists, never `shell=True`.** Pass user text after `--` or `-e` so it can't be read as an option.
- **Every server-side request to a user-influenced URL uses the shared outbound check:** https only, no credentials, and resolved IPs must be global (not loopback, private, link-local, `100.64/10` Tailscale or metadata), with DNS pinned and redirects off. `src/plugin_packages/outbound_notifications/main.rs` is the reference. Validate URLs when they're saved too, even if nothing dials them yet.
