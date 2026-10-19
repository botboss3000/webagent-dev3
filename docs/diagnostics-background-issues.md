# Background issue diagnostics and toast recovery

Implementation: release `9c003595-0cf5-48e4-ab89-1fecaf829b1f`, commit `4be90db6dd22f0177bf11df6f54289b74fec4ead`.

## Configuration and package lifecycle

The installed package owns `plugins/diagnostics/diagnostics.json` and `logs.db`.
Source templates are `config/diagnostics.json` and `src/plugin_packages/diagnostics/diagnostics.json`.
`bridge::folder()` normally resolves `/live-checkout/plugins/diagnostics`; `V3_PLUGIN_TEST_ROOT` is an isolated-test override.
The loop reads settings through the UID-checked socket. Both Rust schemas must agree.

```json
"background_issues": {"enabled": true, "show_toasts": true}
```

`/enabled` is the master capture switch; `/background_issues/enabled` selects browser observations.
`/background_issues/show_toasts` changes presentation independently. Both nested defaults are true;
the checked-in master capture default remains false. Hidden mode retains logging when its separate
authorization/configuration gates permit capture. Visibility never grants capture permission.
`V3_DIAGNOSTICS_ENABLED=false` disables host capture. No new settings editor is added.

Atomically replace installed settings, preserving unrelated fields and ownership. Host settings
and browser presentation each refresh every five seconds. Missing/invalid settings disable capture;
presentation conservatively defaults to visible. Development acceptance restored visible mode.

The API supervisor checks the installed executable every two seconds. A new image does not replace
an already installed executable: install the exact tested artifact explicitly, retain rollback material
and verify the running child identity. UID 1100 must create SQLite/WAL files in the package directory.
Release acceptance repaired its root-owned 0755 directory to ownership 1100:1100 without changing mode.
Probe child PID/executable files as the actual application UID; container root may lack DAC capabilities.

## Independent issues and recovery

`ui/app.js` creates `createBackgroundIssues` and injects it into the authenticated main chat panel.
An episode is keyed by operation/session and has one UUID shared by opening and resolution. Repeated
failures update local text/count. Matching success removes that episode and emits resolution; unrelated
success or clearing action feedback does not. Session selection only filters visibility, never recovery.
Global episodes remain visible across selections. Stop/sign-out clears local state and fences tracker
transport without inventing recovery. The embedded bearer widget retains its existing feedback.

| Operation | Owner and matching success |
| --- | --- |
| `usage` | Usage `refresh`: current-session usage fetch |
| `message_statistics` | Panel `refreshGutterRuns`: current statistics/runs fetch |
| `session_controls` | Controls `load`: successful refresh; explicit mutations propagate refresh failure |
| `shared_controls` | Sharing `load`: successful refresh; existing three-second polling retries |
| `shared_snapshot` | Sharing `onState`: current shared snapshot |
| `chat_sync` | Client `recover`: matching-session snapshot synchronization |
| `history_refresh` | `reopenHistory`: generation-matching background history reopen |
| `read_ack` | `queueRead`: successful acknowledgement; three-second retry while relevant and visible |
| `manager_status` | Review timer: successful manager-state fetch |
| `status_poll` | Shell global status poll: successful status response; see known gap |
| `sessions_list` | `sessions`: successful current list generation |
| `activity_display` | `paintActivity`: next successful relevant render |
| `media_display` | Next successful relevant media display |
| `transcript_position` | Next successful relevant positioning callback |

Display failures retry when their relevant path runs again; autonomous retry is not promised for every
failure. Activity rendering has one lifecycle owner, preventing an outer caller from falsely resolving
a caught render error. Session usage, snapshots, list/history callbacks fence stale generations.
Read acknowledgement resolves before the separate list refresh, which owns its own failure.

## Bounded delivery and authenticated API

The browser retains at most 128 active episodes and a 100-item delivery budget, reserving a resolution
slot for each admitted opening. Delivery is serial and retries transient failures up to three attempts.
Stable episode/event identity permits ACK-lost replay. Rejected/discarded openings suppress later
resolutions. Capture disablement discards queued capture state; re-enabling mid-episode does not
create an unmatched resolution. Stop/start fences in-flight tracker transport.

Logging is bounded observation evidence, not guaranteed delivery or recovery authority. Teardown,
capture disablement, capacity eviction or exhausted transport retries can leave an opening without
a resolution. Retention may remove either row. Missing resolution is not proof of an active failure.

- `GET /api/v3/diagnostics/background/config`: browser authentication; effective caller-specific capture and `show_toasts`.
- `POST /api/v3/diagnostics/background/events`: browser authentication, normal CSRF/origin checks and additional original fixed `admin` administrator authority; 2,048-byte body limit and unknown-field rejection.
- `GET /api/v3/admin/diagnostics?category=app_background`: existing fixed-admin archive reader and bounded pagination.
- `GET /api/v3/admin/diagnostics/status`: existing status plus background switches and config adoption.

Reports accept UUID `issue_id`, `issue_opened|issue_resolved`, an operation from the table, optional
session UUID and HTTP status 100–599, repeat count 1–1,000,000 and duration 0–31,536,000,000 ms.
Local error text stays in the display; raw messages, URLs, headers, credentials and model/tool text
are not accepted observation fields. Another administrator cannot report/read fixed-admin evidence.

The host attaches trusted owner/category/level/config digest. A session found in its caller-scoped
store becomes indexed `session_id`; a missing/deleted session retains only the untrusted
`client_session_id` claim. Neither field grants session access or independently proves recovery.

Reports directly await `bridge::send`, bypassing the host mpsc recorder queue. The standalone process
checks Unix peer identity, current config digest, owner and capture gates, executes SQLite insertion
and sends ACK byte 1. HTTP then returns `accepted:true`; disabled/unavailable capture can return false.
The partial unique index on `(issue_id,event)` for retained `app_background` rows makes duplicate
inserts no-ops that still ACK. Deduplication is not permanent exactly-once storage after retention.
Existing age/row retention and pruning apply.

## Maintainer function map

| Functions | Purpose and verified connection |
| --- | --- |
| `createBackgroundIssues` | Own independent episodes, bounded delivery and injected request/render/time/timer seams |
| `key`, `paint` | Identity and global/current-session visibility respecting presentation policy |
| `release`, `discard`, `clearCapture` | Release reserved capacity and suppress abandoned capture without claiming recovery |
| `event` | Construct bounded content-free payloads and reserve opening-before-resolution capacity |
| `flush` | Serialize retry-safe POSTs, reject stale transport epochs and suppress unmatched resolutions |
| `refreshConfig` | Poll effective policy, fence late config responses, update display and unblock delivery |
| Tracker `fail`, `resolve`, `select` | Open/coalesce, resolve matching episode, select display scope |
| Tracker `start`, `stop` | Poll configuration; cancel/fence/reset capture and presentation at teardown |
| Shell `paintNotices`, `note`, `clearNoticeFrom` | Combine action feedback with independent background messages |
| Shell `boot`, `out`, status callback | Start/stop tracker with authentication; report global status observations |
| Usage `refresh`; controls/sharing `load`, `updateRuns` | Fetch/report matching state and propagate explicit action errors |
| Client `recover` | Synchronize current-session snapshot after transport gaps |
| Panel `refreshGutterRuns`, `paintActivity`, `reopenHistory` | Own statistics, render and background-history transitions |
| `queueRead`, review timer, sharing `onState` | Retry acknowledgements; observe manager state/shared snapshots |
| Panel `sessions`, `open`, `runs`, `disconnect`, `destroy` | Fence generations, trigger refreshes, tear down timers/observers |
| `IssueEvent::validate` | Enforce operation/event/UUID/numeric bounds; schema rejects unknown fields |
| `background::configuration`, `report`, `router` | Expose effective policy; authorize/ACK observations; register bounded/no-store routes |
| `background_router` | Merge browser routes, or empty router when diagnostics is compiled out |
| Both `BackgroundIssues::default` | Align host/package nested defaults |
| `permits`, `status_value`, diagnostics `read` | Apply master/category gates; expose status and archive filtering |
| Standalone `handle`, `open`, `prune` | Authenticate/ACK inserts, initialize archive/dedup schema, enforce retention |

## Evidence and known review finding

Release gates passed default/live compile checks, full backend tests, broker frontend tests and 46
focused UI tests. Runtime checks verified healthy containers/readiness 200, exact child executable
adoption, authenticated API persistence/deduplication in visible and hidden modes, and restoration
to visible. Public reload composed shell, history/activity and composer. Lifecycle fault injection
used actual frontend tracker/notice/render harnesses plus API boundaries, not a live browser fault.

The documentation review freshly reran 39 affected UI tests. It found one remaining issue:
`ui/app.js` global status callbacks lack authentication-generation and request-sequence fencing.
An earlier failure after stop/start can display an old error; an older failed poll can arrive after
a newer success. Reproduce: start tracker; retain old status-failure callback; stop; start; invoke
old callback. The currently started tracker accepts it. A separate correction should fence shell
status callbacks by auth generation and sequence. This pass documents the finding rather than
certifying every callback as stale-safe. Backend review found no material correctness issue.

Durable receipts: `build-jobs/checks/diagnostics-background-current-01a14f8f/evidence/`.
Maintenance is a separate release-bound activity; implementation ownership and engine claims stay closed.

## Complete scoped function inventory

These purpose/contract notes were checked against the full implementation, including
meaningful callable bodies beyond uncertain lexical parser spans. Module declarations
wire the named child implementations; they are not callable functions or new authority.
Test helpers are deterministic fixtures, not proof of live runtime behavior.

### config/diagnostics.json

| Function or type | Verified purpose and contract |
| --- | --- |

### src/api/mod.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `context_config_get` | Read validated compaction configuration from app.compaction_config_path and return configuration plus digest; route is under existing administrator/browser authorization. |
| `context_config_put` | Validate expected digest and candidate compaction configuration, serialize through app.gate, reject concurrent configuration change, persist and return new configuration/digest. |
| `router` | Compose public login/embed/health/static routes, authenticated API routes and administrator routes. Merge diagnostics background routes into browser-authenticated API; apply scoped identity/CSRF middleware, body limits and security/no-store response headers. Read UI roots from V3_UI_ROOT and V3_SHARED_UI_ROOT. |

### src/diagnostics.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `assembly` | Compiled-out fallback returns None without context measurement/capture. |
| `backend_error` | Compiled-out fallback intentionally captures no global backend error content. |
| `background_router` | Compiled-out fallback registers no browser background-observation endpoints. |
| `committed` | Compiled-out fallback records no execution transition. |
| `context` | Compiled-out fallback returns 404 Snapshot unavailable; never fabricates captured context. |
| `failure` | Compiled-out fallback records no provider failure. |
| `http_errors` | Compiled-out fallback passes request through without diagnostic capture. |
| `init` | Compiled-out fallback performs no plugin initialization. |
| `provider_enabled` | Compiled-out fallback returns false so callers avoid provider telemetry preparation. |
| `router` | Compiled-out fallback registers no administrator diagnostics routes. |
| `save_run_observed` | Persist ordinary run transaction first; emit committed telemetry only if successful save advances run_revision. Diagnostics does not determine transaction authority. |
| `save_run_override_observed` | Persist override run transaction first; emit recovery-class committed telemetry only if successful save advances run_revision. |
| `summary` | Compiled-out fallback returns None without summary-input telemetry. |
| `validation` | Compiled-out fallback returns None without validation telemetry. |

### src/diagnostics_plugin/background.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `browser_observations_are_bounded_and_allowlisted` | Verify valid fixture acceptance plus rejection of unknown operation/event, malformed session ID, status range, zero repeats and forbidden raw message field. |
| `browser_reports_retain_auth_csrf_and_fixed_admin_scope` | Exercise complete API routing: unauthenticated 401, absent CSRF 403, original admin reaches handler, invalid operation 400 and a second active administrator receives 403. |
| `configuration` | Return capture eligibility for original admin scope plus available/valid enabled diagnostics, independently return show_toasts; missing/invalid settings conservatively preserve visible toasts. |
| `event` | Construct a valid randomized usage-failure observation for validation tests. |
| `report` | Require original-admin administrator authority and validated observation, check provided session existence in scoped store, preserve unverified client_session_id separately, construct bounded metadata and await standalone persistence ACK before accepted:true; disabled capture returns accepted:false. |
| `router` | Register background config GET and events POST beneath outer browser authentication/CSRF; limit JSON body to 2,048 bytes and mark responses no-store. |
| `toast_visibility_does_not_disable_issue_capture` | Verify hidden toasts still allow enabled background capture, while master/category disable withdraw capture. |
| `validate` | Accept only valid issue/session UUIDs, opened/resolved event names, fourteen operation names, HTTP statuses 100..599, repeats 1..1,000,000 and durations at most one year. Strict input schema excludes arbitrary raw error content. |
| `IssueEvent` | Untrusted strict bounded browser observation; excludes raw message/URL/header content and grants no runtime authority. |

### src/diagnostics_plugin/config.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `active` | Require top-level enabled and absence of exact V3_DIAGNOSTICS_ENABLED=false emergency override. |
| `category_default` | Default omitted run/tool/recovery category flags to true. |
| `default` | Default background category selection and toast visibility to true; existing master enabled remains independently authoritative. |
| `disabled` | Deserialize built-in disabled diagnostics template as safe fallback; malformed compiled template is a programmer error. |
| `expiry` | Parse fixed RFC3339 UTC seconds without another parser dependency; validate Gregorian calendar/time range and return epoch seconds or None. |
| `read` | Read at most 65,536 configuration bytes, deserialize strict schema and validate invariants; return raw-byte-derived digest shared by host and standalone protocol. |
| `scope_defaults_and_windows_are_strict` | Verify built-in capture is disabled, schema scope cannot expand, UTC epoch/date parsing is exact and invalid leap-day/timezone formats are rejected. |
| `selected` | Require active context-assembly/full-context configuration and an exact matching session window whose expiry is in the future. |
| `validate` | Require schema version 1/fixed_admin scope and bounded positive retention/snapshot budgets; reject duplicate/invalid UUID session windows and invalid UTC expiry strings. |
| `BackgroundIssues` | Separate background capture category selector from presentation visibility; neither bypasses master capture or account authority. |
| `Categories` | Per-category existing provider/context/run/tool/recovery capture selectors. |
| `Config` | Strict versioned fixed-admin capture configuration, category selectors, retention limits, selected full-context windows and independent background presentation settings. |
| `FullContext` | Explicit opt-in selected-session encrypted snapshot limits; separate from content-free metadata and background observations. |
| `Retention` | Metadata age and row-count budgets constrained by Config::validate. |
| `Session` | One validated UUID plus fixed-format UTC expiry selecting a temporary full-context capture window. |

### src/diagnostics_plugin/mod.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `assembled_instructions` | Reconstruct base+guidance plus currently selected included contributions for selected encrypted snapshot; selection error returns None. |
| `assembly` | Classify prepared context as ready/compaction-required/failure, obtain bounded failure code/stage, then delegate capture_attempt. |
| `authorize` | Require fixed original-admin scoped store then existing phase3 installation-administrator authority. |
| `backend_error` | Deliberately empty global-error hook; unscoped raw backend content is not captured. |
| `background_router` | Export browser observation router for normal authenticated API composition. |
| `capture_attempt` | Gate scoped context evidence, measure inputs and optionally queue explicitly selected full-context capture. Bound snapshot bytes/queue permits before owned private copies; annotate skipped/oversized/queue statuses and emit content-free attempt metadata. |
| `committed` | Observe only scoped committed execution transitions with available plugin; delegate sanitized transition construction then enqueue. |
| `context` | Require fixed-admin scoped store and present recorder/archive, then read/decrypt exact requested snapshot; absent installation returns 404. |
| `emit` | Try nonblocking insertion into bounded metadata queue; increment dropped counter on full/closed queue without failing caller. |
| `event_allowed` | Require trusted admin owner, exact current configuration digest and category permission before queued event persistence. |
| `failure` | Project sanitized provider failure kind/status/transport and allowlisted recovery action into error metadata; never capture raw error text. |
| `heartbeat` | Send current role status over bridge only when plugin is available; caller records write failures. |
| `http_errors` | Pass request directly through; no broad HTTP content/error collection. |
| `identifier` | Project only nonempty <=128-byte ASCII alphanumeric/_-:. identifiers, otherwise null; prevents arbitrary text entering metadata identifier fields. |
| `init` | Initialize bridge and read-only archive handle once, create bounded event/snapshot queues and memory budget, spawn persistence consumers and five-second settings refresh/heartbeat. Loop settings arrive over trusted plugin IPC; API reads package JSON. Package absence never recreates installation. |
| `insert_snapshot` | Insert encrypted snapshot once per attempt with configured expiry; helper retained for focused database tests rather than production plugin persistence. |
| `measure` | Measure serialized JSON sizes and content-free contribution/compaction boundary statistics using context accounting; sizes are not provider token counts. Return measurements_unavailable if history accounting fails. |
| `no_store` | Add cache-control:no-store to diagnostics responses. |
| `open` | Test-only initialize isolated writable diagnostic schema under root/diagnostics with WAL and private directory mode. |
| `permits` | Require no configuration error, active master and selected category; app_background uses background_issues.enabled independently of show_toasts. |
| `persist` | Send metadata event through existing standalone bridge and await acknowledgement; pool argument is retained interface only. |
| `persist_snapshot` | Recheck selected capture before and after blocking encryption; seal with scoped store and send snapshot metadata/ciphertext over bridge while retaining queue byte permit. |
| `provider_enabled` | Tell provider callers whether fixed-admin scoped, available-plugin provider capture is enabled. |
| `prune` | Apply transactional bounded age/row/snapshot-expiry/total-byte pruning; production archive pruning is owned by standalone package. |
| `read` | Read validated settings and raw configuration digest; invalid/missing configuration produces disabled settings with bounded configuration error code. |
| `read` | Require authority; validate bounded identifier/category/level filters; query current read-only archive with parameter binding, admin-owner filter and page limit 1..500; missing archive yields empty disabled response. |
| `read_snapshot` | Fetch only matching attempt/session/run and unexpired encrypted row; decrypt using caller store and recheck sealed owner/identity fields before returning. |
| `reader` | Open lazy read-only logs.db pool with two connections and bounded busy timeout; fresh read pools follow replaced package inode rather than initialize archive. |
| `router` | Register existing administrator log/status GET endpoints with no-store response layer. |
| `scope` | Restrict capture to store.current_user_id()==admin; administrator role alone does not widen capture. |
| `seed` | Test-only create-new fixture template writer; create parent directories and preserve an existing file unchanged. |
| `settings` | Clone recorder configuration under read lock; missing recorder/failed lock returns None. |
| `snapshot_allowed` | Require event_allowed and current explicitly selected unexpired full-context session window. |
| `status` | Require authority and report archive counts plus process heartbeat freshness; distinguish absent recorder/archive and current process state. |
| `status_value` | Project current configuration, availability, queue depths and drop/write/skip counters for status/heartbeat; include background settings. |
| `summary` | Classify summary-input preparation result and delegate capture_attempt with fixed summary stage/failure code. |
| `transition` | Map supported committed event kinds to tool/run/recovery category and allowlisted state/phase/reason metadata, revision/lease counters and projected tool failure; omit unknown event kinds or disabled categories. |
| `validation` | Emit scoped enabled context-validation failure metadata with attempt linkage and bounded caller-supplied static failure code/stage; return event to caller. |
| `Capture` | Borrowed full-context snapshot serialization view, instantiated only after capture gates and bounded measurement; encrypted before standalone archive write. |
| `Filter` | Strict optional administrator diagnostic query filters with bounded validation/pagination. |
| `Recorder` | Singleton read pool, current settings, bounded event/snapshot channels, memory semaphore and observation counters. |
| `Settings` | Host snapshot of validated config, matching digest and bounded load-error status. |
| `Snapshot` | Owned selected private snapshot with scoped encryption store and semaphore permit held through processing. |

### src/plugin_packages/diagnostics/config.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `active` | Require top-level enabled and absence of exact V3_DIAGNOSTICS_ENABLED=false emergency override. |
| `category_default` | Default omitted run/tool/recovery category flags to true. |
| `default` | Default background category selection and toast visibility to true; existing master enabled remains independently authoritative. |
| `disabled` | Deserialize built-in disabled diagnostics template as safe fallback; malformed compiled template is a programmer error. |
| `expiry` | Parse fixed RFC3339 UTC seconds without another parser dependency; validate Gregorian calendar/time range and return epoch seconds or None. |
| `read` | Read at most 65,536 configuration bytes, deserialize strict schema and validate invariants; return raw-byte-derived digest shared by host and standalone protocol. |
| `scope_defaults_and_windows_are_strict` | Verify built-in capture is disabled, schema scope cannot expand, UTC epoch/date parsing is exact and invalid leap-day/timezone formats are rejected. |
| `selected` | Require active context-assembly/full-context configuration and an exact matching session window whose expiry is in the future. |
| `validate` | Require schema version 1/fixed_admin scope and bounded positive retention/snapshot budgets; reject duplicate/invalid UUID session windows and invalid UTC expiry strings. |
| `BackgroundIssues` | Separate background capture category selector from presentation visibility; neither bypasses master capture or account authority. |
| `Categories` | Per-category existing provider/context/run/tool/recovery capture selectors. |
| `Config` | Strict versioned fixed-admin capture configuration, category selectors, retention limits, selected full-context windows and independent background presentation settings. |
| `FullContext` | Explicit opt-in selected-session encrypted snapshot limits; separate from content-free metadata and background observations. |
| `Retention` | Metadata age and row-count budgets constrained by Config::validate. |
| `Session` | One validated UUID plus fixed-format UTC expiry selecting a temporary full-context capture window. |

### src/plugin_packages/diagnostics/diagnostics.json

| Function or type | Verified purpose and contract |
| --- | --- |

### src/plugin_packages/diagnostics/main.rs

| Function or type | Verified purpose and contract |
| --- | --- |
| `background_protocol_is_retry_safe_and_visibility_independent` | Prove opened/resolved duplicate retries ACK yet retain two rows with hidden toasts; stale configuration digest, master off and category off reject further capture. |
| `getuid` | Declare libc real UID primitive used to require Unix socket peer UID to match the standalone process user. |
| `handle` | Handle one bounded versioned Unix frame from same-UID caller. Reload strict package configuration; supply configuration, upsert role heartbeat, or gate admin event/snapshot by current digest/master/category. Persist before ACK=1; duplicate background episode/event insert is ignored but acknowledged. |
| `main` | Resolve package/state roots, initialize private runtime directory and archive, bind private Unix socket, maintain atomic readiness heartbeat, serialize bounded connection handling and periodically prune using current configuration. |
| `now` | Return current Unix seconds for archive timestamps, expiry and readiness heartbeat. |
| `open` | Initialize package logs.db with private mode, WAL/two connections, archive tables/indexes and unique app_background(issue_id,event) index; schema ownership remains in standalone plugin. |
| `protocol_rejects_public_owner_and_unknown_version` | Exercise real UnixStream frames to reject foreign owner/version and oversized frame while accepting one enabled provider event. |
| `prune` | Transactionally prune at most 1,000 rows per age/count/expiry/byte criterion using current configured retention; all metadata categories share archive retention. |
| `submit_background` | Test helper sends one background frame through paired Unix streams and requires ACK byte1 for each successful handling result. |

### tests/chat-controls.test.cjs

| Function or group | Verified purpose and contract |
| --- | --- |
| `Element` | Minimal DOM/button/event/dialog fixture supporting actual session controls and notification modules; test-only behavioral stub. |
| `fixture` | Instantiate actual controls/notification modules in VM with mutable server snapshots, captured writes/errors and optional issue adapter. |
| `tick` | Yield one event-loop turn to let deferred control actions settle. |

### ui/app.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `password-error-feedback` | Feature group for inline credential rejection and form accessibility; security feedback is not hidden by diagnostics preference. |
| `api` | Perform same-origin JSON API calls with cookie credentials and CSRF on writes; preserve error HTTP status and sign out on non-login 401. |
| `applyAccountTheme` | Apply selected/system light mode and current theme button ARIA/active state. |
| `applyAgentTabs` | Deduplicate/filter saved favorite page IDs, retain Config, take revision and synchronize tab order. |
| `boot` | Load identity/CSRF/chat config, finish optional embed authorization, start tracker and account pages, redeem invitation and select conversation, then start 3s status polling. |
| `closeAccountMenu` | Hide account menu and reset account-button aria-expanded. |
| `closeMobileNav` | Hide mobile navigation and reset its toggle aria-expanded. |
| `disposeSessionsPage` | Increment sessions-page generation, destroy its mounted instance and clear its shell view class. |
| `end` | Nested pull-refresh completion handler: cancel unarmed gesture, reload normal pull or invoke sign-out/browser purge for hard pull. |
| `hardRefresh` | Serialize browser-data reset; optional sign-out must succeed before purge, then force cache-busting navigation. |
| `loadAccountControls` | Mount available models and load session policy/sign-ins; render revocation controls and local failure status. |
| `loadAgentRoster` | Read /models/agents, select an accessible fallback when necessary, update route and render cards. |
| `loadAgentTabs` | Mark preferences unready, request current user favorites and apply revisioned snapshot. |
| `loadGuestPolicy` | Read access mode to show guest login/approval text; failure hides guest login. |
| `loadMainPages` | Read page catalog, create desktop/mobile tabs and mounts, load public assets and initialize page modules before reflecting selected state. |
| `mountAgentModels` | Replace role-model selection table; retry inaccessible requested agent with shared_default and render local failure if unavailable. |
| `mountAgents` | Fetch and validate agent shell markup/catalog, construct configuration tabs/model host and sessions region, then normalize the route. |
| `mountModels` | Nested mountAgentModels adapter binding request/agent plus agent-change and assignment callbacks to role/model selection table. |
| `note` | Update only action feedback and its source; suppress Sign in required text and preserve background issue state. |
| `out` | Stop background capture, disconnect chat and account-specific pages/polling, reset authenticated state and show private overlay. |
| `paintNotices` | Join action notice with current visible independent background messages into textContent; never interprets notice text as markup. |
| `passwordError` | Create/reuse inline password error, attach aria-describedby and aria-invalid, and preserve credential errors outside toast preference. |
| `pending` | Nested async startProtectedPage asset-loading promise, followed by local page failure rendering; not a separate exported API. |
| `positionAccountMenu` | Position body-portaled account menu near current desktop/mobile anchor, clamped to viewport. |
| `privateVisible` | Apply restricted/private overlay visibility and reset its credential-form reveal control. |
| `refresh` | Fetch caller-specific /user/status or admin /status, update model/provider/policy/run projections; errors propagate to caller. |
| `renderAccount` | Store authenticated identity, apply admin/guest page/control visibility and populate account profile identity fields. |
| `renderAgentCards` | Render caller-discovered agents and Sessions card; serialize agent-selection requests without rebinding the open conversation. |
| `request` | Nested saveAgentTabs queued write; suppress stale identity/generation work and restore preferences after failure. |
| `saveAgentTabs` | Queue revision-fenced favorite writes with captured user/generation; reload authoritative preferences after a write failure. |
| `selectAgent` | Change configuration agent and route, render cards and remount role/model views without reopening the current conversation. |
| `setupHeaderPullRefresh` | Install top-25px-band mobile pull gesture with normal reload at 50% viewport and hard refresh/sign-out at 85%; preserve ordinary scrolling. Immutable released baseline has pullZone=25. |
| `showAccountPanel` | Delegate account navigation to showMainPage using supplied history mode. |
| `showAgentView` | Select an allowed configuration panel, update ARIA/tab/route state, and fetch Identity or Abilities as selected. |
| `showMainPage` | Validate main page/admin Builds access, update visible page/module/session/account lifecycle and browser route/mobile view. |
| `showMobileView` | Select main/chat mobile surface and update chat control/page state. |
| `showSessions` | Normalize Sessions route and mount page once; destroy a late mount if scope/generation changed. Opening a row delegates to chat.open. |
| `startProtectedPage` | Start authenticated module assets once per page; failed HTML/import/start remains local to the page and permits retry. |
| `syncAgentTabs` | Reorder/hide favorite tabs and render configuration directory whose favorite actions persist through saveAgentTabs. |
| `syncLayout` | Apply mobile/desktop/floating layout, chat and resize visibility, and navigation/control projections. |
| `updateChatControl` | Reflect chat visibility in shell toggle active class and aria-expanded. |
| `updateMainPageState` | Apply active main page visibility, tab classes/ARIA selection and native select value. |
| `updateMobileNavigation` | Set mobile back label and selected page styling from current main page catalog. |

### ui/chat/controls/session-controls.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `chat-run-controls` | Feature group linking composer control presentation config to session-owned mode/model authority. |
| `choose` | Send revision-fenced next-turn model preference with client request UUID, then refresh current-generation state. |
| `close` | Hide model picker and reset expanded ARIA state. |
| `cycleMode` | Choose next permitted execution mode, confirm widening when required, send expected revision and refresh authoritative state. |
| `cycleModel` | Load current controls, cycle only connected permitted models, delegate revisioned write to choose. |
| `load` | Coalesce current session controls read, fence stale generation, resolve issue after successful projection; log failures, clear state for 404, propagate other failures. |
| `mountSessionControls` | Mount session mode/model controls, server capability projection, revisioned mutations and issue-aware refresh lifecycle. |
| `paint` | Project server mode/permitted models and running model into button text/tooltip; disable while busy/unready/read-only. |
| `ready` | Require writable conversation and a successful fresh controls load before mode/model action; cached state cannot bypass failed read. |
| `show` | Render/position permitted model picker; disable disconnected models and preserve pointer composer focus. |

### ui/chat/controls/shared-chat.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `chat-shared-controls` | Feature group for configured header placement/visibility with runtime-authoritative invitations, help and participation permissions. |
| `disconnect` | Invalidate old reads, clear sharing state/invitation URL and collapse owned share UI without claiming issue recovery. |
| `load` | Refresh sharing data for current generation, project state to panel and resolve shared_controls; report current-session failure and propagate. |
| `mountSharedChat` | Build Share/help/participation controls, register them with chat layout and poll visible selected-session sharing state every 3s. |
| `mutate` | Serialize one sharing action, refresh authoritative current-generation state afterward and release busy UI in finally. |
| `paint` | Project server access/policy, participation/help and invitation lifecycle into controls and local status; presentation does not grant rights. |
| `ready` | Ensure conversation exists and load fresh sharing state before action; reject missing state. |

### ui/chat/controls/shared-chat.test.cjs

| Function or group | Verified purpose and contract |
| --- | --- |
| `Node` | Minimal DOM fixture for sharing controls; implements only required node/attribute/child operations. |
| `setup` | Load actual shared-chat module in VM with layout-registration stub, capture controls and timer, inject test client/issue adapters. |
| `state` | Build caller sharing-policy/access/participation snapshot with revision 7 for controlled tests. |

### ui/chat/controls/usage-stats.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `chat-usage-context` | Feature group distinguishing provider-reported tokens/cost from serialized context bytes and linking presentation/compaction configuration. |
| `close` | Hide statistics dialog and clear selected-run context without changing session usage. |
| `formatCount` | Format finite token/count values as raw/k/M strings; preserve missing values as em dash. |
| `mountUsageStats` | Mount context statistics strip/dialog and session usage refresh lifecycle from measured server data; owns events/portal teardown. |
| `paint` | Project recorded usage/context/cost into strip and configured visibility; repaint dialog only when it does not own focus. |
| `paintPanel` | Render measured context bytes, provider usage/cost and available compaction settings; validate positive safe-integer admin limits against config digest before saving. |
| `refresh` | Coalesce/throttle usage reads at 1.5s, fence generation, preserve previous data on failure and resolve only after successful current render. |
| `refreshConfig` | Read administrator context limits for optional dialog; omit unavailable config without failing usage display. |
| `row` | Append text-only label/value row to context dialog. |
| `toggle` | Open/close and position stats dialog, optionally pin run, force usage/config refresh. |

### ui/chat/core/client.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `chat-transport-recovery` | Feature group for epoch/sequence/history-revision fencing and bounded snapshot/SSE recovery; no new durable retry configuration. |
| `createChatClient` | Construct authenticated session API adapters and epoch-fenced SSE transport; caller supplies request/auth and presentation adapters. |
| `deliver` | Validate sequenced public message, deduplicate ID, require accepted rendering before advancing local cursor. |
| `disconnect` | Invalidate transport epoch, cancel current retry, close SSE and stop administrator notices. |
| `open` | Fetch initial snapshot, render only current epoch, create SSE with last sequence and bind session/queue/message/reconnect callbacks. |
| `recover` | Coalesce snapshot gap recovery, reject stale sessions/history rewinds, merge messages/queue state and resolve chat_sync after successful recovery; bounded exponential retries on failure. |
| `setConnecting` | Reflect live chat connection state in shell toggle title/class when toggle exists. |
| `updateRuns` | Observe selected session status transitions; terminal change triggers snapshot recovery to restore missing final publication. |
| `valid` | Accept state callback only while it remains current and matches transport epoch. |

### ui/chat/core/panel.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `chat-panel` | Feature group for whole mounted conversation transcript/composer lifecycle, layout profiles and authenticated action wiring. |
| `chat-screen-panel-routing` | Feature group passing selected-session live runs/messages into screen-awake display policy; history loading does not signal fresh attention. |
| `main-chat-main-notes-ui` | Feature grouping for optional agent Main Notes mounted into header and selected from session agent during snapshot load; excluded when enableMainNotes=false. |
| `native-session-messaging-controls` | Feature grouping for session messaging-route controls and queue access, composed with server-authoritative sharing/capabilities. |
| `button` | Build text button whose action uses caller guard. |
| `cancel` | Require control access, request idempotent run cancellation and refresh shell in finally. |
| `components` | Render allowlisted display_only component descriptions as text; never convert model choices/forms/approvals into actions. |
| `destroy` | Dispose child views/listeners/observers/timers and callback properties after disconnect; invalidate panel generation. |
| `disconnect` | Clear selected-session visuals and disconnect all owned session transports/control adapters; navigation filters issues without logging recovery. |
| `ensureSession` | Coalesce conversation creation with stable receipt and reject if selected-session generation changes before opening result. |
| `execute` | Nested historyAction request adapter reusing the same pending body/receipt when retrying a network failure. |
| `failedRunMessages` | Render/remove transcript notices for definite failed runs; uncertain effects remain recovery UI. |
| `historyAction` | Run undo/fork using displayed sequence and stable client operation identity; fence session changes, reconcile retry result and preserve user draft choice. |
| `historyChanged` | Detect higher history revision; defer while history mutation busy or reopen current history. |
| `message` | Insert public message by sequence with ID deduplication, update existing admission state, observe reads and delegate activity rendering. |
| `mountChat` | Fetch required chat HTML partial, validate required IDs and replace root; malformed shell fails mount. |
| `mountChatPanel` | Mount/replace one chat root with transport, transcript, composer, controls and authenticated shell adapters; own destruction and returned panel API. |
| `newSession` | Create/list/open new conversation only when message/upload not pending; return created session ID. |
| `open` | Select route/session and reset scoped views, load snapshot/live transport, connect sharing/media/messaging, restore focus only while initiating focus/generation unchanged. |
| `paintActivity` | Render selected-session run activity; central owner opens activity_display on exception and resolves only after successful render. |
| `queueRead` | Debounce highest visible message acknowledgment, fence current read generation, resolve immediately after successful ack and retry failure in 3s while eligible. |
| `refreshGutterRuns` | Load current-session snapshot for run statistics; session/generation fences issue opening and resolution. |
| `rememberRuns` | Cache newest run snapshots for gutters and trim oldest beyond 100. |
| `reopenHistory` | Perform background replace-mode reopen with session/generation-fenced history_refresh failure/resolution, separate from explicit undo/fork feedback. |
| `resetReads` | Invalidate pending acknowledgment callbacks, clear timer/cursors/visible rows and disconnect read observer. |
| `runs` | Fan selected run state into transport/progress/activity/usage/controls, composer availability, approval/recovery/resume/cancel UI; server permissions remain authoritative. |
| `selectedRun` | Resolve run only from selected conversation, otherwise reject action. |
| `sessions` | Read session listing with generation fencing, update native/dropdown projections only for newest request and track sessions_list. |
| `shouldFocusComposer` | Read per-profile new/existing-session keyboard focus policy with appropriate defaults. |
| `submit` | Validate composer state, ensure session, send stable-identity message plus attachment/context data, merge snapshot and preserve drafts across async work; refresh final state. |
| `syncSnapshot` | Merge matching session/history revision only, update access, messages/followups and authoritative runs; trigger reopen on changed revision. |
| `undoChoice` | Ask undo confirmation and explicit draft handling; settle once and remove owned dialog on choice/cancel/teardown. |
| `updateChatClearance` | Measure header/composer and set CSS transcript clearance custom properties; ResizeObserver drives updates. |
| `watchFocus` | Track focus movement during asynchronous load; expose unchanged predicate and listener cleanup. |

### ui/chat/core/transcript-recovery.test.cjs

| Function or group | Verified purpose and contract |
| --- | --- |
| `EventSource` | Synthetic transport collecting listeners, close state and explicit message delivery for deterministic recovery tests. |
| `fixture` | Load actual chat client with synthetic SSE, mutable snapshots, captured publications/timers and failure injection. |
| `open` | Fixture adapter opening actual client and recording publication/metadata/snapshot outcomes; supports injected render failure. |
| `reopenHistory` | Parser false-positive at source-string lookup in test, not a declared test function; actual reopenHistory is extracted from panel.js and reviewed separately. |
| `snapshot` | Build minimal public messages/run snapshot and maximum sequence for fixture recovery. |
| `tick` | Yield event loop for outstanding recovery requests/callbacks. |

### ui/shared/background-issues.js

| Function or group | Verified purpose and contract |
| --- | --- |
| `clearCapture` | Invalidate active/queued capture and reset reservations/queue on authorization/config loss; do not falsify UI recovery. |
| `createBackgroundIssues` | Create independent per-operation/session episode tracker and ordered bounded browser-to-diagnostics event transport; display and capture are separate. |
| `discard` | Mark episode uncaptured, release its reserved slot and remove queued events when opening declined/exhausted. |
| `event` | Admit opening only when capture enabled/started and queue capacity can reserve eventual resolution; serialize bounded metadata without raw error text. |
| `flush` | Serialize immutable episode/event bodies; retry at most three times with stable identity, suppress unmatched resolutions and isolate old lifecycle completions by token/epoch. |
| `key` | Form operation/session composite identity; episode UUID changes after true resolution/new failure. |
| `paint` | Render configured-visible global and selected-session messages only; does not resolve hidden-session issues. |
| `refreshConfig` | Read effective capture/presentation policy at most one in flight, fence lifecycle responses, repaint current issues and flush only after config ready. |
| `release` | Release one reserved future-resolution queue slot exactly once. |

### ui/shared/background-issues.test.cjs

| Function or group | Verified purpose and contract |
| --- | --- |
| `api` | Parser false-positive inside extracted shell source marker, not a declared function in this test; actual app.js api and shell integration are reviewed separately. |
| `setup` | Load actual tracker in VM with capture/presentation config, deterministic IDs/clock, recorded events/messages and fake timers. |
| `tick` | Fixture event-loop barrier allowing asynchronous config/delivery operations to settle. |
| `transportFixture` | Load actual tracker with injectable config/delivery promises, recorded requests and manual retries for bounded transport tests. |

### Meaningful inline and returned callbacks

- `ui/shared/background-issues.js`: {"fail": "Open/deduplicate episode while started; cap count at 1e6 and active map128; display local error with operation label.", "resolve": "Remove only matching episode, emit captured resolution and repaint.", "select": "Filter visible session, no false resolution.", "start": "Idempotent config read/5s polling.", "stop": "Invalidate epoch/timers/queue/map and clear rendered issue notices."}
- `ui/chat/core/client.js`: Returned endpoint wrappers are documented inline: queue delivery/resume, undo/fork, sharing/invites/participation/help, usage/context config, snapshots/attachments/voice, controls/mode/model, session lifecycle/read/create/send and run decisions/recovery/resume. They delegate method/path/body unchanged through caller-authenticated request; no new independent authority.
- `ui/chat/core/panel.js`: Returned panel methods validate draft/context/run ownership and delegate to submit/cancel/decide/recover; accessor reports selected session. UI event handlers use guard and server capability projections. read observer, reviewTimer, sharing onState and open transport callbacks own matched failure/success paths.
- `ui/app.js`: Inline handlers cover authentication/register/profile/password/session-policy/revocation, provider/model/policy writes, main/agent/mobile/theme navigation, shell chat widget and pull refresh; credential errors stay inline and explicit actions retain note feedback.

Configuration assessments distinguish actual JSON consumers from protocol, identity,
accessibility and deterministic-fixture invariants; missing settings are opportunities,
not an assertion that diagnostics configuration controls every function in a touched file.
