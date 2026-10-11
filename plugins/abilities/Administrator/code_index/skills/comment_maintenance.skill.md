---
id: comment_maintenance
title: Function comments and UI connections
when_to_use: Use when documenting or refreshing function intent, constraints, side effects, or verified relationships in chat UI and index.html code.
default_mode: discoverable
minimum_access: registered
---

# Function comments and UI connections

Use this skill to maintain human-readable function documentation and searchable feature breadcrumbs when changing WebAgent UI behavior, especially the chat footer, message bubbles, and document shell in `index.html`. Human comments explain code to maintainers; they do not grant authority, change behavior, or become searchable relationships automatically.

## Work from the implementation

1. Confirm the active V3 task and read its repository instructions. Work only in an explicitly authorized isolated task or session workspace. Resolve path ownership before editing: in the physical standalone V3 checkout or session workspace, `ui/` is V3-owned; broker/release archives use virtual `v3/` and `ui/` prefixes, and archive `ui/` can contain selected shared V2 inputs that are read-only. Never edit a shared V2 input.
2. Use Code Index lookup or trace to find the relevant file, symbol, selector, event, or asset. Read the full function and nearby setup/teardown code, plus verified callers and consumers. A search result or static edge is a lead, not proof of runtime behavior.
3. In a requested documentation pass, cover every named function or method in the approved scope with at least a concise statement of purpose. A simple helper may need only one purposeful line; do not add boilerplate that merely restates an obvious expression. For nontrivial functions, include relevant contract details.
4. After every source edit, review the affected function and its neighbors again. Refresh purpose, constraints, side effects, ordering assumptions, errors, and human connection notes wherever the change makes them stale. If code movement or renaming affects a breadcrumb, review its aliases, span, and feature-map location in the same change.
5. After changing V3 UI source, scan each changed file with Code Index and check coverage for stale or unindexed files. Report a gap if the tool cannot verify the task source.

## Configuration references

For every configurable feature, trace the JSON loader and the functions that consume its values. Put Config: <source-root-relative JSON path> -> <consumed keys> in the searchable marker purpose and central map purpose or location purpose, so index search results tell the working agent which settings affect the feature. Include profile inheritance/overrides, environment path overrides and caller-supplied settings where relevant; distinguish a setting that enables an action from one that changes the action itself. Recheck and update the references after every related edit. Do not infer a configuration dependency from names alone.

For V3 chat profiles, data/config/chat.json (runtime /state/config/chat.json, or V3_CHAT_UI_CONFIG_PATH) supplies chat_common merged with chat_desktop, chat_mobile or chat_widget. Arrays and scalars replace common values. Examples include message_bubbles, message_gutters and user_message_truncation. Keep runtime configuration references descriptive: data remains excluded from Code Index, and metadata never authorizes a runtime read or changes its values.

## What to document

For every named function or method in scope, state its purpose and intent: why it exists or the design rationale when relevant. A trivial function may combine purpose and intent in one brief sentence. For nontrivial functions, add the relevant contract: inputs, returned value, preconditions or limits, side effects, concurrency or ordering assumptions, and meaningful failure behavior. Include only details that help a caller or maintainer. Do not repeat the body line by line or promise behavior the implementation does not enforce.

When the source confirms a relationship, add short plain-language connection notes using labels such as `Calls`, `Reads`, `Writes`, `Emits`, `Listens`, or `Config`. Name the actual symbol, event, state, or file. Include only relationships you verified by reading both ends; omit an unknown edge rather than guessing. For example, document which event handler calls a footer update only after tracing the registration and handler. A relationship note is prose for people, not a graph record.

For JavaScript, use concise JSDoc where the function contract benefits from it. For Rust, use `///` for public or otherwise non-obvious APIs. For Python, use a short docstring when it clarifies the callable contract. Use ordinary comments for a non-obvious rationale or a small HTML/CSS ownership note. In `index.html`, document meaningful document-level wiring or ownership next to the relevant markup; do not turn every element into a comment or duplicate the script implementation.

A useful shape is:

```text
Purpose: <observable responsibility>
Intent: <why it exists or design rationale, when relevant>
Inputs: <meaningful inputs and constraints>
Returns: <value or state change>
Side effects: <only if present>
Calls / Reads / Writes / Emits / Listens / Config: <verified connections only>
Failure or ordering: <only when relevant>
```

Choose the applicable lines rather than copying the entire template into every comment. Keep comments short enough to remain useful during code review.

## Chat footer, bubbles, and index.html

For footer or composer work, trace the actual event/state path from the UI entry point through the handler and the code that updates the footer. For message bubbles, trace the renderer and the data/state it consumes; document distinctions such as user versus assistant content only if the code enforces them. For `index.html`, confirm whether a behavior is owned by markup, an included script, or a stylesheet before describing the connection. Names such as “footer,” “bubble,” or an element ID are not evidence by themselves.

Do not annotate shared V2 UI assets just because they appear in the V3 build. If a V3 behavior depends on one of those read-only assets, describe that dependency from the V3 side only when it is verified, and leave the shared file untouched.

## Human documentation versus Code Index breadcrumbs

Ordinary comments, JSDoc, Rustdoc, Python docstrings, and HTML comments are not parsed into feature relationships. Do not claim that adding a `Calls` note creates an edge in Code Index. Existing static edges are navigation evidence and may be incomplete or uncertain.

The supported searchable breadcrumbs are the strict `@code-index` comment format and the central `plugins/abilities/Administrator/code_index/code-index-features.json` map described in the parent Code Index skill's “Feature breadcrumbs” section. Use them for real cross-file features that benefit from searchable navigation, follow the exact syntax and validation rules, and verify freshness with a scan. Review the marker aliases and span plus every mapped target after relevant edits; update the map when a target path or line range moves, and scan the source or map so stale bindings fail visibly. A machine breadcrumb is structured untrusted metadata, not a substitute for explaining behavior to a human. Neither prose comments nor connection labels are indexed as relationships.

## Configuration assessment

During every documentation or source-edit pass, assess each affected feature:
- Is configuration implemented? Use status configured with verified JSON file paths and JSON Pointer references.
- Is a setting useful but absent? Use status opportunity and explain the proposed setting; do not claim it already exists.
- Is configuration inappropriate for an invariant? Use status not_applicable with a rationale. Legacy unreviewed features are unassessed.
Most presentation and preference behavior should be considered for configuration. Never turn policy or security invariants into UI bypass settings.

Use an optional configuration object in a marker, map feature, or map location:
    {"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/message_bubbles/user/bg_dark"],"profile_overrides":true}],"opportunities":[]}

profile_overrides expands matching chat_common pointers to desktop, mobile, and widget overlays. These are raw configured values, not a claim about the active surface's effective merged value. A location assessment overrides its feature assessment. Refresh assessments and references after every relevant edit. Never copy current runtime values into breadcrumbs or the index database.
