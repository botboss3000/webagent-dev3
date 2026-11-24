# Drop-in abilities and live publication

The Rust engine discovers validated immutable catalogs at request/model-call
boundaries. New isolated abilities require no main-app compilation. Publish through
the protected broker; source directory edits alone are not installation.
The first general-loader migration requires an engine release.

## Tool folders for compiled abilities

A compiled ability declares each tool in its own folder,
`abilities/<Group>/<ability_id>/tools/<tool>/`, and leaves `tools` out of
its manifest; a packaged ability whose package contract checks the manifest list keeps
it, naming exactly its tool folders. `<tool>.tool.json` holds:

- `name` (the folder name), `description`, JSON-schema `parameters`, optional `strict`.
  Keep `description` to one sentence of at most 160 characters (what the tool does and
  when to use it): it rides every call. Field formats, limits and examples go in the
  tool's `<tool>.skill.md`.
  Every call is checked against `parameters` before the tool runs
  (`crates/modules/engine/src/tools/arguments.rs`), so a schema may use only the keywords
  that check enforces: `type`, `properties`, `required`, `additionalProperties` (a boolean
  or a schema), `items`, `enum`, `minimum`/`maximum`, `minLength`/`maxLength`,
  `minItems`/`maxItems`, `pattern`, `format: "uuid"` and `anyOf`, plus `description`,
  `title` and `default`. Asset validation refuses any other keyword. When the broker
  passes the live release (`--validate-baseline`), an asset-only release, which ships no
  Rust code, may change descriptions, limits and patterns and remove allowed values, but
  may not add, remove or rename an argument, change a type, add an allowed value, make a
  required argument optional or remove a tool definition. Tools of a plugin package whose
  code changes in the same release are exempt, as are `build_work` and `performance_work`,
  whose arguments the build broker validates itself.
- `effect`: `read` or `write`. It says whether the tool changes anything and drives
  recovery (an interrupted write is uncertain and never rerun), batches, background work
  and the repeat guard. It never decides access.
- `task` (optional): `required` or `exempt`, whether the task gate applies (the session
  task list must account for every user request, with a step in progress, before the call
  runs). Absent: a core tool is exempt, otherwise a write is required and a read exempt.
  Mark coordination and housekeeping writes `exempt`. It decides only when the agent's
  Task Progress setting `task_gate` is `changes`; the default, `all_tools`, covers every
  non-core tool. See [tool-policy.md](tool-policy.md)
  for the whole decision path.
- `default_policy` per role (`operator`, `member`, `visitor`): `allow`,
  `require_approval` or `deny`. A level with no saved permission for the tool uses its
  role's value, then the next role down. With no role named, a read allows and a write
  requires approval. Agent operators can change it on the Abilities page.
- `owner_tier` (optional): the tiers whose owners' agents may have the tool, with every tier
  ranked above them. It overrides the ability's list.
- `role_access` (optional): the built-in roles that may ever have the tool, for example
  `["operator"]`. A hard limit: no level setting widens it, and the tool is hidden from
  other roles. Without it the ability's list applies, then every role. A custom level
  counts as the built-in role it starts from.
- `core` (optional, default false; null counts as false): `true` offers the tool
  in every session without loading it, and skips ability and tier gating. Its own
  policy still applies. Base's harness tools (loaders, background control, chat panels,
  session search, attachments and compaction) set it.
- `repeat_guard` (optional): `risk` (`none`, `low`, `high`; a write defaults to high),
  `hint` (what the agent verifies before repeating) and `warning` (what the user is told).
- `usage_hint` (optional): one line added to argument errors to guide the next call.
- `retention` (optional): what stored calls keep once the conversation has moved past them
  (behind the compaction fold, or in a session idle for the storage policy's
  `trim_idle_days`). `output` is `disposable` (re-runnable lookups: only the outcome
  fields stay), `log` (outcome, short fields, `keep_fields`, and long text cut to its last
  `tail_lines` lines) or `record` (kept whole; the default). `drop_input_fields` names
  arguments replaced by their size, such as a written file's content. A read whose `path` a
  later call in the same pass wrote is trimmed as disposable whatever its level. A trimmed
  output records `original_sha256` and `original_bytes` of what it replaced.
- `critical` (optional, default false): the action ledger records every call with the user
  requests it answers, and the record outlives its session; stored calls are never trimmed,
  so a critical tool takes no `retention` level or `drop_input_fields`. Only writes may set
  it. Before each call the critical tool guard checks the user asked for it. See
  [tool-policy.md](tool-policy.md) sections 7 and 8.
- `guard` (optional, critical tools only): `{"check": "...", "show": [...], "hide": [...]}`
  adds details the critical tool guard must match (`check`, up to 300 characters) and names
  parameters it always sees in full (`show`) or never sees (`hide`). It can only add to
  the guard's rule. See [tool-policy.md](tool-policy.md) section 8.

`<tool>.skill.md` is its optional guidance. The handler stays compiled Rust and lists the
tool in its adapter's `TOOLS`. At startup every listed handler must have a valid folder
and every folder a handler; a missing or invalid folder disables only that tool. Every
tool an ability lists has a `<tool>.tool.json`; a tool not built yet keeps a skeleton (empty
`parameters`, no handler) so its access is declared.
Abilities not yet moved keep the manifest `tools` list and flat `tools/<tool>.skill.md`.

## Skill modes

Tools and skills have one of four modes: `core` (always in the prompt, locked; set only by
`"core": true` in `<tool>.tool.json` or `<id>.skill.json`), `loaded` (in the prompt without
a request), `on_request` (the agent loads it when needed) and `off`. With nothing saved a
tool or skill is `on_request`. Saved data written before these names (`visible`,
`discoverable`) is rewritten once at startup.

- An ability's own skill `skills/<id>.skill.md` may have `skills/<id>.skill.json` beside it:
  `{"core": bool, "default_mode": "loaded" | "on_request" | "off"}`, both optional and no
  other keys. `core: true` makes the skill core; `default_mode` is its mode when nothing is
  saved. A change counts as a skill edit for the ability's identity.
- A selectable skill `skills/<name>.skill.md` sets its default in front matter:
  `default_mode: loaded` or `default_mode: on_request` (the default).

## Ability access defaults and app tiers

An ability's manifest may declare `default_policy` per role, like `<tool>.tool.json`. It is
shown on the Abilities page ("Default: …") but grants nothing: a level with no saved
permission for the ability (and no `*`) keeps it off, so a new ability stays off for
existing agents until an Operator switches it on. Once on, its tools without a saved
permission use their `<tool>.tool.json` defaults and its skills their `default_mode`.

Every ability manifest must declare `owner_tier`, the app tiers whose owners' agents may
have it (for example `["free", "administrator"]`, or `["plus", "administrator"]` for a
Plus-only ability); a manifest without it is reported and the ability works for no one.
A tier also gets every ability a tier ranked below it gets (the access policy's
`tier_order`, highest first), so a listed tier grants every tier above it and a new tier
needs no manifest change; an unknown tier counts as the lowest one. The agent owner's tier
decides: an ability the owner's
tier lacks stays offered but its tools fail with the tier reason (its saved settings are
kept and return on an upgrade). A `<tool>.tool.json` may declare its own `owner_tier`, which
overrides its ability's list; a tool several abilities provide works through any the
owner's tier allows. A manifest may also declare `role_access`, a hard limit on which
built-in roles may have the ability. Administrator-group abilities must list exactly
`["administrator"]` and are hard-coded to need an administrator owner and an
administrator caller (`crates/modules/abilities/src/ability_access.rs`,
`crates/modules/access`). A new paid tier gets no abilities until manifests list it.

The running app compares `/code/abilities` with the published release
it runs (`crates/modules/abilities/src/drift.rs`, rechecked at most every five seconds).
Edits to a tool's description or `default_policy`, and changed guidance text
(`<tool>.skill.md`, `<id>.skill.md`, `skills/*.skill.md`), apply at once with no notice.
A removed tool or ability, an invalid `<tool>.tool.json`, or a `<tool>.tool.json` whose name,
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

An isolated package lives at abilities/<Group>/<ability_id>. Retain the
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
        "assets": ["example.json", "skills/example.skill.md", "schemas.json"]
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
abilities/Core/automation/plugin.json. Its standalone crate and complete
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
reject new calls. An installed, enabled package keeps its tool schemas in the model's
tool list whatever its health; a call made while its health probe is failing or stale
(probes run every 60 seconds and stay valid for 90) fails with an error instead. Installation changes rotate activation identity, invalidate
health and deactivate retained work. Legacy copied files remain rollback
material and cannot become an executable fallback.

The initial extraction requires a coordinated app release for the native host
registration. Subsequent compatible package publications use plugin_deploy
without replacing API/loop/gate. External event adapters, clone runners and
device execution remain unavailable. This section documents the candidate
contract; release receipts and live acceptance establish installation status.

Index: docs/index.md — update this doc's row there whenever you change it.
