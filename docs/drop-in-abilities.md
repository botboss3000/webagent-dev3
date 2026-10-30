# Drop-in abilities and live publication

The Rust engine discovers validated immutable catalogs at request/model-call
boundaries. New isolated abilities require no main-app compilation. Publish through
the protected broker; source directory edits alone are not installation.
The first general-loader migration requires an engine release.

## Tool folders for compiled abilities

A compiled ability declares each tool in its own folder,
`plugins/abilities/<Group>/<ability_id>/tools/<tool>/`, and leaves `tools` out of
its manifest; a packaged ability whose package contract checks the manifest list keeps
it, naming exactly its tool folders. `tool.json` holds `name` (the folder name),
`description`, JSON-schema `parameters`, optional `strict`, `effect` (`read` or
`write`) and `default_access` per role (`operator`, `member`, `visitor`: `auto`,
`ask` or `deny`); `tool.skill.md` is its optional guidance. The handler stays compiled Rust and lists the tool in its adapter's
`TOOLS`. At startup every listed handler must have a valid folder and every folder a
handler; a missing or invalid folder disables only that tool. A capability level with
no saved permission for a tool uses `default_access` for its role, then the next role
down, then deny. Built-in Base tools are compiled in (`crates/modules/abilities/src/base.rs`)
with coded defaults: Operators get them all, Members and Visitors may ask for read tools.
Abilities not yet moved keep the manifest `tools` list and flat `tools/<tool>.skill.md`.

## Ability access defaults and plan entitlements

An ability's manifest may declare `default_access` per role, like `tool.json`. In a saved
capability ladder, a level with no saved permission for the ability (and no `*`) uses
that default for its role, then the next role down, then deny; an ability that declares
none stays closed until a level grants it, and an unconfigured ladder keeps every
ability closed. Account plans (tiers) grant by entitlement group: the
`entitlement_group` in the group's `_group.json` names it, and a tier's `entitlements`
map grants it. An ability or tool the tier does not name, and that `*` does not cover,
inherits its group's grant, so a new ability in a granted group works in that plan at
once. Named grants always win, and an upper tier still caps the tiers below it
(`crates/modules/abilities/src/ability_access.rs`, `crates/modules/access`).

Administrator native tools (`webagent_administration::tools::TOOLS`) use the same
folders under `plugins/abilities/Administrator/`. Tools with no catalog ability (the
in-app broker, terminal and tunnel tools) live in the retired `v3_build`,
`v3_workspace`, `terminal_control` and `remote_tunnel` folders, which the catalog does
not list. Each catalog snapshot reads these schemas from its own folders, so a published
description edit needs no Rust build; release validation refuses a missing or invalid
Administrator folder.

Automation, a packaged ability, keeps its manifest `tools` list and also defines each
tool in a folder; its `plugin.json` lists every `tools/<tool>/tool.json` as an asset, so
a definition edit changes the package's runtime identity but not its build key.

## Plugins that call the app (`requests_v1`)

A Rust ability plugin can ask the app to act for it. Its `plugin.json` sets
`"host_interface": "requests_v1"`, `"protocol": 2`, the tools it serves (a subset of its
tool folders) and `requests`, the host requests it uses, chosen from the offered set in
`crates/modules/extensions/src/requests.rs` (fixed user-API routes, the caller's own
skills, the ability's settings and session watches). Every request runs as the chatting
user with that user's permissions, after the app re-authorizes the tool call; a request
the plugin did not declare is refused, and route parameters cannot leave their path
segment. Agent Management (`plugins/abilities/Core/agent_management`) is the first.

A plugin that needs external services declares them in `providers` with
`web.provider_request` (`crates/modules/extensions/src/providers.rs`): each provider has
a fixed HTTPS origin, fixed endpoint methods and paths, and optionally a vault
credential the app adds to a declared header. The sandboxed plugin has no network; the
app makes each call as the chatting user, never returns the key, and counts each user's
monthly use against a cap the plugin passes. Web Search (`plugins/abilities/Web/web_search`)
is the first.

The plugin is one standalone crate with no workspace dependencies (the root workspace
excludes its folder). Dev releases build it in the broker's plugin lane and run each call
in its own sandboxed process over JSON lines (`crates/modules/extensions/src/plugin.rs`,
runner in `crates/modules/extensions/host/plugins.rs`). Stable releases link it into the
binary through the root Cargo feature `stable-abilities`.

## Checkout edits

The running app compares `/live-checkout/plugins/abilities` with the published release
it runs (`crates/modules/abilities/src/drift.rs`, rechecked at most every five seconds).
Edits to a tool's description or `default_access`, and changed guidance text
(`tool.skill.md`, `<id>.skill.md`, `skills/*.skill.md`), apply at once with no notice.
A removed tool or ability, an invalid `tool.json`, or a `tool.json` whose name,
parameters, effect or `strict` changed turns that tool off at once. Added abilities and
tools stay inactive, and changed code or manifests keep the last build running. Every
change that needs a rebuild is listed in an administrator-only popup
(`/api/v3/admin/ability-drift`); dismissing it hides that change set until the next login
or, with "Remind me at next login" unchecked, for good. A new change set shows again.
The popup's Rebuild button (`POST /api/v3/admin/ability-drift/rebuild`) asks the broker to
release the live checkout as it is (`v3_build_rebuild_checkout`): a dev rebuild publishes
changed ability packages without a restart, and a stable release is a full build that links
the ability plugins (`stable-abilities`) and restarts the app briefly. Only the installation
administrator may rebuild; the broker commits what it released.

## Isolated packages

An isolated package lives at plugins/abilities/<Group>/<ability_id>. Retain the
existing <ability_id>.json declaration format, own .skill.md, tools/ guidance
and skills/ selectable guidance. Add plugin.json and schemas.json:

    {
      "id": "example",
      "dispatch": "isolated",
      "protocol": 1,
      "interface": 1,
      "runtime": "python3",
      "entrypoint": "runner.py",
      "schemas": "schemas.json",
      "effect": "write",
      "tools": ["example_echo"],
      "build": {
        "toolchain": "image-python3",
        "inputs": ["runner.py"],
        "dependencies": [],
        "flags": [],
        "assets": ["example.json", "example.skill.md", "schemas.json"]
      }
    }

Tool schemas use the provider function shape: type=function, name, description,
and parameters. Parameters support closed objects, arrays, strings, integers,
numbers, booleans and null; required, enum, numeric bounds and length/item bounds.
References, dynamic schema composition and unknown keywords are rejected.
Tool names cannot collide with native/private controls or another ability.

Protocol 1 sends one bounded JSON request on stdin:
protocol=1, invocation_id, tool and arguments. Return one JSON response containing
the same protocol/invocation_id, status=success|failed|uncertain and output. The process runs
with only its published package and current session workspace, no application
state, encryption keys, credentials or control sockets. Environment variables
are supplied by the trusted bridge. Python uses isolated mode; load package-local
helpers explicitly. Timeouts, invalid frames and process failures conservatively
produce uncertain write results. Use failed only for an authoritative known failure; report uncertain when effects cannot be established. Reconcile before retrying. Generic interface 1
classifies every invocation as a potential write, including pure computations;
new tool policy defaults deny until an administrator explicitly grants it.
Background execution is limited to existing vetted native tools.

A standalone Rust crate uses runtime=executable, build.toolchain=rust-1.99,
a simple underscore identifier as entrypoint, and explicit compile inputs including
Cargo.toml, Cargo.lock and every Rust source/include. Dependencies and flags must
be empty in this first manifest interface. Cargo's locked dependency graph belongs
in the lockfile. Builds run offline without application state, credentials, sockets
or network; required crates must already exist in the build cache. Unavailable
dependencies require controlled provisioning. Content-addressed executable
artifacts live outside source releases; identical build inputs reuse artifacts.
A package edit does not compile its Rust callers. Prebuilt executable packages
can instead include the reviewed executable in their declared inputs.

Each model call pins catalog, schemas, guidance and published prompt defaults.
Current grants and revocations are rechecked before effects. Admitted calls retain
their original implementation; removing a package blocks new calls. Old snapshots
and artifacts remain available for recovery and rollback. Visible guidance and
previously selected bundled guidance update at subsequent model calls; agent-owned
skill selections retain their explicit revision fence.

Source configuration templates publish independently and never overwrite saved
runtime configuration. Published prompt defaults merge with saved overrides,
using the first migration's per-leaf default hashes as provenance. Existing
naming jobs keep their settings snapshots; new jobs use current resolved prompts.

GET /api/v3/admin/configuration lists live configuration owners/adoption boundaries.
GET/PUT /api/v3/admin/configuration/<id> reads and publishes configuration with
expected_digest and config. Invalid and stale saves preserve current bytes.
Existing domain APIs remain supported, including package-owned Diagnostics.
Scheduler/cache limits adopt at the next tick/request; lowering admission limits
does not cancel existing runs. Encryption mode is reread before admin writes;
existing mixed-format records and the key are preserved.
GET /api/v3/admin/publication/status reports bounded API/loop catalog adoption.
Broker completion requires both processes to acknowledge the published revision.

Root operators may use lifecycle.py reconfigure --config CURRENT
--candidate-config CANDIDATE --task-id TASK [--dry-run]. Both configuration files
must satisfy the existing protected ownership checks. Reserve an idle maintenance
drain first. Changing the host listener replaces only API; changing the public
origin replaces API and loop. Retained images are reused, rollback containers and
configuration are preserved, and no compilation runs. Storage/socket relocation
and identity changes are rejected before effects; they require a separate explicit
migration rather than implicit movement of runtime data. Resume the broker after
checking readiness and the reconfiguration receipt.

Existing native adapters remain supported. This release does not turn pending
legacy declarations into implemented tools or extract every native handler.
A new engine capability or incompatible protocol still needs an engine update.

## Native Automation ability boundary

Automation uses the fixed host_interface=automation_v1 registration under
plugins/abilities/Core/automation/plugin.json. Its standalone crate and complete
test suite live in service/, alongside the package descriptor, schemas and UI.
The application Cargo manifest does not build an Automation binary, and its
image does not copy an Automation distribution. Installations can omit this
ability directory while retaining the core loop and generic background tasks.

This native reducer protocol differs from generic isolated model plugins.
The app owns caller scoping, policy, approvals, namespace CAS, durable events,
background admission and terminal publication. The reducer receives bounded
host-selected state and metadata through stdin, with host-enforced filesystem,
network and subprocess confinement. A declaration cannot select another native
host interface or provide trusted caller identity.

Builds use the locked offline standalone crate and its complete test suite.
Content-addressed artifacts resolve from the active immutable publication.
Compatible implementation updates preserve activation identity and schedules;
admitted calls retain their pinned executable. Missing or disabled packages
reject new calls. Installation changes rotate activation identity, invalidate
health and deactivate retained work. Legacy copied files remain rollback
material and cannot become an executable fallback.

The initial extraction requires a coordinated app release for the native host
registration. Subsequent compatible package publications use plugin_deploy
without replacing API/loop/gate. External event adapters, clone runners and
device execution remain unavailable. This section documents the candidate
contract; release receipts and live acceptance establish installation status.
