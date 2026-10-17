# Live model-call context

The context release resolves permanent instruction identities against current
sources at every model-call boundary. Prompts, skill bodies and tool schemas
participate in that refresh; selecting a skill does not freeze its text.

## Execution route and ownership

The normal loop in src/agent_loop/run.rs hydrates native selection identities
through src/context/assets.rs::hydrate, resolves current catalog and live policy,
and assembles the permitted schema set. prepare_capability reads current prompt
defaults and saved agent slots, then calls the capability-context planner.
project obtains safe placements and schema order from context-assets.
The host validates both results before context assembly and provider dispatch.
Dispatch fingerprints and a synchronous source fence recheck source/authority
after credential retrieval and before the network call. Repeated changes are
bounded and pause for review rather than dispatching stale context.

src/live_assets.rs::resolved_prompts_at merges published config/prompts.json
defaults with saved data-root config/prompts.json overrides, using saved default
provenance. Provider::instructions_for_snapshot composes base_instructions,
closing_reporting and jev_guidance. Agent prompt slots are host-owned encrypted
records, read again by prepare_capability. The legacy last-good provider helper
is not the strict dispatch freshness authority. Publishing defaults does not
overwrite explicit saved overrides.

src/storage/context_assets.rs keeps encrypted conversation selections scoped by
agent and requester in the canonical conversation owner's user plane. Services
receive validated snapshots and compute plans; only the host commits them with
run, lease, participation and revision fences. Selections grant no permission.
Loading through native ability/tool/skill controls stages an explicit selector;
automatic dependencies are resolved from current authority, not made permanent.
The native Base context_assets tool lists selectors and their current revision.
Unload requires the exact selector, expected_revision and stable request_id.
It changes selection only; configured visible guidance can remain present.

## Placement, compaction and cache behavior

Permanent selections store identities, layout anchors, schema order and bounded
idempotency receipts. They do not store frozen instruction bodies. Current full
skill bodies are retained without preview truncation. Temporary menus and trusted
Call/Turn contributions are rebuilt at their lifetime boundaries.

For append profiles, surviving definitions retain safe anchors; new definitions
append after a completed exchange. Temporary native context follows the latest
safe boundary and does not inherit permanent placement. Schemas retain surviving
order and append new permitted names deterministically. Non-append profiles use
their declared instruction prefix. The host accounts for serialized synthetic
growth once, including provider-specific representation.

Synthetic instructions are a model-call projection. They never enter durable
chat history or the history summarized by compaction. Compaction and Undo reset
layout metadata while keeping selectors and receipts, so the next call rebuilds
current definitions. Unload is revision-fenced; stale saves or retries cannot
resurrect a removed selector.

Stable unchanged bytes and ordering can preserve a provider's reusable prefix.
Changed live sources deliberately change those bytes. Actual cache hits, billed
tokens and provider latency are not guaranteed by placement tests. Tool schemas
remain in each endpoint's native request field; this feature does not move them
into durable transcript text.

## Provider-specific projection

provider-projection emits bounded references into host-supplied items. The shared
contract validates coverage, ordering and identity; the host reconstructs content
and retains endpoint, credential, transport and publication authority.

| Provider profile | Placement and replay |
| --- | --- |
| OpenAI subscription | Responses items, developer instruction placement, subscription session cache key |
| DeepSeek | Chat-completions system placement, implicit prefix caching, original DeepSeek reasoning replay |
| Other installed compatible chat endpoints | Their existing chat-completions prefix profile, without foreign reasoning fields |
| Typesafe | Decision profile; ordinary chat projection is rejected |

These are installed protocol profiles in crates/provider-contracts, not
model-selectable formatting or permission overrides. No new endpoint is enabled
by a component result.

## Drop-in components and configuration

| Component | Shared contract / implementation / service | Operation |
| --- | --- | --- |
| context-assets | assets-contracts / context-assets / assets-service | assets.plan v1 |
| capability-context | capability-contracts / capability-context / capability-service | capability.prepare v1 |
| provider-projection | provider-contracts / provider-projection / provider-service | provider.project v1 |

Crate package names have the webagent- prefix. Each descriptor under
plugins/engine declares the corresponding service, pure implementation, contract,
engine-service transport, Cargo.toml and Cargo.lock as build inputs. Each has no
runtime component dependency. Limits are 16 MiB request and response, 30 seconds,
and one concurrent request per service. The private-pipe limit includes framing.
The provider contract currently allows a payload up to the same 16 MiB value,
so envelope overhead creates a small linked/live discrepancy near that ceiling.
The review found this boundary mismatch; exact maximum-size equivalence is not
claimed and needs a separate functional correction. A service generation is reused; each
ability does not require a new host connection.

Default linked-context builds call implementation libraries directly. Live builds
disable defaults and enable live-context, resolving independently sealed service
artifacts. Compatible implementation replacements use the engine publication
lane. Contract/registration changes require coordinated consumer validation.
Implementation versions are 0.1.1; the context-assembly interface is version 2.

Runtime data-root config/engine-components.json owns revisioned availability;
source config/engine-components.json initializes only an absent file. The
authenticated engine-component controls fence runtime and catalog revisions.
Deliberate withdrawal pauses accepted work at the relevant component boundary,
without a linked fallback. Restoration rechecks authorization and resumes only
eligible host-marked component pauses. Manual pause, cancellation and uncertainty
remain stopped.

## Verified release evidence and limits

Main release 8a7c2d23-c936-4471-bbeb-d4aa8624aa83 is committed as
4a8a1c558927a457b3ee3cad9548dda3db7e3719. Schema correction
ef29ca27-0a2e-43c9-a74d-a453ff53d1cd is committed as
4b02cf8cf99a7566d04250629550a8d51e57117f. Independent replacement
94bbf01f-d3b1-443e-82b0-d4477b6c57aa is committed as
014c9481787a7455c6a641471bd54456b1226afb.

Final release be5e2765-66c3-4c12-b88c-6108b6475d3d, commit
69389a4942ee9ed40ac84b508d02bf4c9df0c09c, passed 941 tests with five ignored,
both early feature profiles and optimized linked/live proofs. All six service
artifacts were reused. API/loop/gate health, exact source promotion, live prompt
refresh, conversation retention, compaction, unload, per-component pause/resume,
and manual/cancel negative controls passed. Separate descriptor withdrawal and
restoration passed for each new component. No production or v2 change is claimed.

Exact receipts and API proofs are under
build-jobs/coordination/checks/0aff5b55dbbd9e373305015ef269e15defce4a2cdeed1d9997d330640c824c1e/context-lifecycle/.
The full separately published code_verification skill was hash-verified unchanged.
An earlier transient session-creation HTTP 500 was reconciled through its
persisted request; its cause remains unconfirmed. Provider-wide live billing,
cache-hit and performance measurements are not claimed.

Documentation and index maintenance use a separate user-authorized activity
bound to immutable release/source identities. A current-source review does not
retroactively certify a different historical snapshot. Keep implementation tasks
closed and use the supported maintenance activity API for release indexing and
review claims. See engine-modularization.md for the extraction ledger.
