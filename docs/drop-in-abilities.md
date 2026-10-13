# Drop-in abilities and live publication

The Rust engine discovers validated immutable catalogs at request/model-call
boundaries. New isolated abilities require no main-app compilation. Publish through
the protected broker; source directory edits alone are not installation.
The first general-loader migration requires an engine release.

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

A standalone Rust crate uses runtime=executable, build.toolchain=rust-1.90,
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
