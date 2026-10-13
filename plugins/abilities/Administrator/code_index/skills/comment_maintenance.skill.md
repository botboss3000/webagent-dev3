---
id: comment_maintenance
title: Function comments and UI connections
when_to_use: Use when documenting or refreshing function intent, constraints, side effects, or verified relationships across source files and meaningful feature groups.
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

## Shared maintenance procedure

Use an existing authorized isolated broker task. List live pending/needs_review records, filtered by task, scope or feature; inspect claims and remaining-work notes. Synchronize that task after source edits, and page all affected records. Claim one bounded function or feature group and retain the returned opaque claim ID. Even agents using the same operator account have separate claims. Claims expire after 30 minutes and abandoned work remains visible; use the original request ID to recover a lost claim receipt. An expired claim can be reclaimed against the latest revision.

Trace implementation and both ends of relevant connections. Assess configuration as configured, opportunity or not_applicable with rationale; verify paths and JSON Pointers for configured behavior. Update human comments and searchable metadata before scanning. Sync refreshes task-source evidence but never asserts documentation completion. Record all five checks: documentation (purpose/intent/constraints), connections, breadcrumbs, configuration, index_freshness. Use verified or not_applicable with a bounded rationale; freshness must be verified. Parser uncertainty requires explicit human inspection of the file for meaningful functions/groups, not treating a fallback scan as complete semantic analysis.

Record complete only for current source/dependency hashes, with your claim, current expected_revision and no remaining work. Complete every active child before the file summary. A removed unit requires reconciliation kind deleted or renamed; a rename names a current replacement entry and does not certify that replacement. Stop by releasing with concrete remaining-work notes. Do not declare affected coding work complete until these reviews and the index are current. Publication reports incompleteness without blocking.

### WebAgent examples

Use task_id from your existing isolated build task, never another caller's task_key. These calls use the normal runtime's authenticated identity and authority:

```text
code_index_maintenance(action="sync", task_id="chat-documentation", scope="ui/chat", request_id="chat-sync-0001")
code_index_maintenance(action="list", task_id="chat-documentation", status="pending", limit=50)
code_index_maintenance(action="claim", task_id="chat-documentation", entry_id="<returned entry_id>", expected_revision=<returned revision>, request_id="chat-claim-0001")
code_index_maintenance(action="record", task_id="chat-documentation", entry_id="<claimed entry_id>", expected_revision=<claim revision>, source_hash="<claim source_hash>", claim_id="<claim.id>", status="complete", checks={"documentation":"verified","connections":"verified","breadcrumbs":"verified","configuration":"verified","index_freshness":"verified"}, configuration_assessment={"status":"opportunity","opportunities":["Expose the verified presentation preference in the owning chat profile"]}, remaining_work=[], request_id="chat-review-0001")
code_index_maintenance(action="release", task_id="chat-documentation", entry_id="<claimed entry_id>", expected_revision=<current revision>, claim_id="<claim.id>", remaining_work=["Trace the mobile profile override before certification"], request_id="chat-release-0001")
```

The record/release examples are alternatives. Copy actual IDs, hashes, revisions and assessments from your review; never copy the opportunity assessment without verifying it. Repeating a mutation requires the same request ID and identical input. A revision conflict requires rereading and reviewing the changed evidence, then using a new request ID. After an uncertain result inspect the original receipt/state before issuing another mutation.

Published entries have new IDs in the published namespace; never reuse a task entry ID for a published get. Discover the matching entries with code_index_maintenance(action="list", root="published", scope="v3/plugins/abilities/Administrator/code_index/adapter.py", status="complete", limit=5), then get each returned entry_id with root published and no task_id. Published source paths retain the archive v3/ prefix; private task paths do not. The live canonical ledger exposes the same published IDs with task_id published and logical paths without v3/. Match source_hash, review_task_id, reviewer and publication.release_id when verifying promotion. A zero-result page is not coverage evidence. Follow next_offset when a bounded list has_more, or use exact get once an ID is known.

### Server CLI examples

External agents require server access and the existing protected broker operator channel. Run on the authoritative server checkout; honor its source-edit/build workflow. The CLI creates no public API, MCP listener or new credentials. Mutation payloads are structured JSON files, not shell-embedded JSON:

```sh
python3 -B plugins/abilities/Administrator/code_index/maintenance_cli.py list --task-id chat-documentation --status pending --limit 50
python3 -B plugins/abilities/Administrator/code_index/maintenance_cli.py sync --input /tmp/chat-sync.json
python3 -B plugins/abilities/Administrator/code_index/maintenance_cli.py claim --input /tmp/chat-claim.json
python3 -B plugins/abilities/Administrator/code_index/maintenance_cli.py record --input /tmp/chat-review.json
```

Sync input example:
```json
{"task_id":"chat-documentation","scope":"ui/chat","request_id":"chat-sync-cli-0001"}
```

Claim/review JSON uses the same fields as the WebAgent examples. The operator may provide the returned task_key to review an authorized WebAgent task, including its task_id; this does not change source editing permissions. Reviewer attribution derives from Unix peer identity. Use WEBAGENT_BUILD_CONFIG only when the installation requires an explicit existing protected configuration path. Published reads use root published; live reads show shared ownership and task evidence. Do not edit the ledger SQLite file directly.

Rename receipts bind the replacement file, unit, source hash and metadata dependencies. Publication rebinds its entry ID to the published namespace and invalidates a reconciliation when its replacement changed during rebase. The replacement itself still needs its own review.
