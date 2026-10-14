---
id: live_plugin_publication
title: Independent plugin publication
when_to_use: Choosing publication for abilities, executable plugins, guidance and runtime assets.
default_mode: discoverable
minimum_access: registered
---

# Live plugin and non-Rust publication

Use this skill when creating or editing abilities, executable plugins, Markdown guidance or documentation, JSON manifests/configuration/schemas, UI assets, Python workers, or other non-Rust files. Choose publication from the actual consumers and dependencies, not the file extension.

## Intended architecture

The development direction is independently publishable, versioned ability/plugin bundles and runtime assets. Compatible guidance, manifests, schemas and configuration should update without rebuilding core Rust. Executable abilities should use a stable isolated-process plugin protocol so a compatible plugin can be installed or replaced independently. This is a design direction, not a claim that all existing abilities already support hot reload, and not authorization to implement the migration during an unrelated edit.

Keep core runtime authorization, caller identity, approval enforcement, cancellation, recovery and publication fencing authoritative. Plugin declarations never grant their own permissions or bypass installation/account/session ceilings.

## Independent app components and efficient builds

The broader goal is drop-in parts across the application, not just hot guidance. Changing one component should rebuild and replace only that component and any proven dependents, while unrelated components retain their binaries, images and running instances.

Define components around real ownership and lifecycle boundaries: core API/agent loop, executable abilities/plugins, isolated worker, UI bundles, gates and protected broker where applicable. Prefer independently versioned processes or packages with explicit contracts. Splitting Rust code into crates can improve dependency caching but does not by itself allow replacing part of one running monolithic binary; genuinely independent deployment requires a runtime/process or equivalent supported module boundary.

Give each component a build manifest containing its source inputs, dependency closure, interface version, toolchain, build flags, required runtime assets and output identity. Use content-addressed artifacts and caches keyed by all relevant inputs. Compile only affected components and their transitive build dependents; retain unchanged compatible artifacts. Shared interface/dependency changes can legitimately affect several components, so never promise that every edit affects exactly one part.

Separate build dependencies from runtime dependencies. A stable protocol-compatible plugin implementation update should not trigger recompilation of its callers. Protocol or schema changes require compatibility validation against retained consumers; incompatible changes need a coordinated release or a versioned transition. Configuration/guidance updates should not invalidate executable caches unless they are genuine compile inputs.

For an implementation task, first establish the current dependency graph and a representative build baseline. Introduce one component boundary, then demonstrate an isolated edit rebuilding only its intended artifact. Verify retained consumers continue working, compare actual binary/image/container identities, and measure build duration/cache reuse before claiming efficiency gains. Preserve bounded correctness checks for unchanged components where the interface impact warrants them; do not substitute filename filtering for dependency analysis.

## Determine the current release path

Before editing, identify each file's producer and consumers: API, loop, worker, plugin process, protected broker, browser and startup loader as applicable. Check whether it is embedded at compile time, copied into an image, read at startup, read per call, or resolved through a trusted published revision. Inspect current implementation; update this baseline when architecture changes.

General protocol-1 ability publication now uses the protected plugin_deploy
lane. The engine loads complete immutable catalogs and guidance at request/model
boundaries; new isolated packages can supply new tool schemas and executable
dispatch without compiling the main application. Existing native adapters remain
supported. Python packages are validated in the retained interpreter; standalone
Rust packages compile independently to content-addressed artifacts. The installed
engine validates catalogs before activation; both API and loop must acknowledge
adoption. See docs/drop-in-abilities.md for the exact package/protocol contract.

Source JSON templates publish without overwriting existing runtime settings.
Published prompt defaults merge with saved overrides. Runtime configuration uses
validated revision-fenced domain/admin APIs and explicit adoption boundaries.
Scheduler and cache limits reload; infrastructure changes use scoped operator
reconfiguration with retained images. Storage/socket relocations remain explicit
migrations. New tool IDs require grants; unsupported legacy declarations stay
unavailable. New engine capabilities and incompatible protocols still need an
engine release.

For a narrow edit, use the existing supported lane. If an image release is currently required, explain that the requirement comes from packaging/loading rather than Markdown or JSON compilation. Do not broaden a documentation task into a loader refactor. Do not force a hot lane, bypass the broker, or write directly into a frozen release.

## Release classification

| Changed input | Intended release |
| --- | --- |
| Ordinary Markdown documentation | Checked source promotion; runtime publication only when a runtime consumes it |
| Ability/tool/selectable skill guidance | Validate and atomically publish a compatible bundle; no Rust compilation once the loader supports it |
| Compatible JSON manifests, schemas or runtime configuration | Validate against the installed interface and publish through the owning configuration/bundle mechanism |
| HTML, CSS, JavaScript and static UI assets | Asset publication, with a frontend build only if the actual asset pipeline requires it |
| Executable plugin | Test and build/package that plugin as needed; retain compatible core binaries |
| Python worker or executable scripts | Validate/test and replace or restart the owning executable package through its supported lane |
| Core Rust or Rust compilation inputs | Compile affected binaries and release them |
| Dockerfile, OS packages, runtime dependencies or toolchain | Appropriate image/package rebuild and restart |

This table describes the target separation. Where today's broker lacks a supported lane, follow its current image workflow and report the gap. Rust inputs include Cargo.toml, Cargo.lock, build.rs, generated code and include_str!/include_bytes! assets; a non-.rs change can require Rust compilation. Conversely, a new Rust-based plugin may require compiling only that plugin.

## Migration plan

1. Generalize trusted guidance publication to every ability's own, tool and selectable skills. Make both API and loop consume the same revision at the next model-call boundary.
2. Add validated catalog reload for manifests and schemas. Publish complete immutable candidate bundles atomically; malformed or incompatible candidates retain the previous valid revision. Update revision-bound discovery, activation selectors and context caches deliberately.
3. Define a stable executable-plugin protocol covering identity, version compatibility, tool schemas, invocation, results, permission checks, cancellation, timeouts and durable recovery. Move handlers behind isolated processes incrementally; no arbitrary in-process dynamic library loading is required.
4. Establish independent app component boundaries, build manifests and content-addressed artifact reuse; rebuild only affected dependency closures. Split broker release decisions into source-only, asset/configuration, plugin-package, core-binary and image/environment lanes based on dependency impact. Preserve isolated tasks, review, synchronization preflight, unchanged-input checks, receipts and rollback.
5. Migrate existing abilities in bounded slices. Porting a pending legacy ability still requires implementation and acceptance; hot installation alone does not make it ready.

## Adoption and verification

For live publication, stage and validate a complete candidate, bind it to an immutable revision, then atomically activate it. Agree on the revision across actual consumers before treating a release as complete. Avoid a partially copied live directory as the activation mechanism.

Adopt guidance/catalog snapshots at model-call boundaries; bind each invocation to its validated schema/implementation revision. Keep current authorization checks before effects, including revocations. For removal or incompatible updates, define draining/cancellation/recovery behavior rather than silently changing an in-flight invocation.

Declare each configuration setting's adoption point: immediately, next request/model call, next run, or process restart. A published configuration does not imply every consumer has adopted it. Keep runtime state/secrets separate from source templates and never overwrite existing configuration merely to publish a default.

Verify relevant additions, updates, removals, invalid candidates, compatibility rejection, rollback, active runs, recovery and permission revocation. Check actual API and loop consumers; metadata or a host-file hash alone does not establish new model context. For a no-compile release, verify the broker lane and unchanged application images/container identities as appropriate.

Report separately what was edited, published, adopted, compiled/restarted and behaviorally verified. Use the checkout AGENTS.md and Codebase Admin task workflow; this skill adds no per-release approval requirement and never expands the requested scope.

## Engine component extraction ledger

Before substantial engine work, read docs/engine-modularization.md and update its component hierarchy, crate/process boundary, build/runtime dependencies, rationale, rejected alternatives, measurements and verified release progress. Context assembly is the first pilot; do not proactively split all remaining engine code. Engine packages use plugins/engine separately from model-callable abilities. Shared contracts and incompatible interfaces require a coordinated core release; compatible registered implementations use the independent broker lane. Deliberate removal pauses affected work with no silent linked fallback. Maintain both linked stable and isolated live behavior checks, generation pinning, authority validation and Code Index review.
