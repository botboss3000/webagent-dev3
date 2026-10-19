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

## Exact duplicate tool results

Every call executes and saves its complete original output. Suppression changes
only model-facing context. Match the same tool, canonical JSON inputs and
host-resolved session/agent/requester/workspace scope, then compare complete
original result text exactly. Object keys are canonicalized for input identity;
result timestamps, array ordering, domain IDs and embedded result metadata are
not normalized. Interaction-row metadata and transport correlation are separate.
Hashes aid lookup; exact equality and host validation remain mandatory.

Unrelated calls never evict earlier results. Changed results and errors reset
only their matching sequence; errors stay full. The first full result remains
the primary until compaction. Later successful exact repeats become references
only when their serialized replacement is smaller. Recorded call order controls
selection even in parallel batches. Assignments persist across turns, restart and
provider changes. Toggle/allowlist/template edits affect future observations;
previously emitted placeholders stay stable until compaction.

Successful compaction promotes the latest surviving occurrence in each unchanged
sequence to full. Earlier survivors may reference it forward, but every reference
points directly to a full result in that assembled context. Folded changes/errors
remain reset barriers. The host reconstructs and verifies authorized originals,
expands summary evidence, and pauses if provenance cannot be verified. Recall
returns stored original output; it never moves primaries or rewrites placeholders.
Ordinary preview/envelope limits remain separate; no provider cache-hit, billed
cost or latency improvement is claimed by these correctness tests.

### Controls and required live prompt

Identity's **Suppress identical tool results** defaults enabled. Each tool's
Abilities panel independently includes/excludes it through **Suppress identical
results**. Default inclusion is file_read, file_list, file_search, v3_source_read,
v3_source_list, code_index_lookup, code_index_coverage, code_index_trace and
code_index_progress; all other tools default excluded. Inclusion grants no
permission and never skips execution. Lifetime/budget eviction remains separately
preserved as **Under development · Experimental — disabled**; effective forces
that policy off while retaining saved settings.

Identity uses /api/v3/models/agents/{id}/identity with revision. Tool inclusion
uses /api/v3/agents/{id}/abilities/{ability}/tools/{tool}/result-deduplication with
expected_revision and included. Manager authority, tenant scope, catalog identity
and stale-write rejection remain host checks. Agent tool_result_deduplication
policy and tool_result_deduplication_tools overrides have separate persisted
namespaces. The ability API projects the tool map as tool_result_deduplication.

Config: data/config/prompts.json -> /tool_result_deduplication/placeholder.
Source config/prompts.json ships the required value. dedup_template reads the
actual live data-root JSON at each model-call boundary; it does not consult
embedded defaults or alternate text. Supported variables are {current_call_id},
{reference_call_id} and {tool}. Empty text, malformed braces, unsupported variables
and templates over 8192 bytes are rejected. The regular nonsymlink JSON file is
bounded to 4 MiB. Revision-fenced live prompts writes validate before saving and
need no Rust compilation. Enabled suppression with missing/invalid configuration
pauses before model dispatch. New placeholders capture current validated text;
compaction can use captured assignment configuration when policy is later off.
That captured configuration is not a hardcoded/alternate-template fallback.

### Planner and host connections

| Functions / source | Purpose and boundaries |
| --- | --- |
| context-contracts/tool_results::canonical, key, fingerprint | Canonical input/scope identity and exact-text fingerprints; never normalize result content. |
| validate_template, replacement | Validate required syntax, substitute variables once and compare serialized sizes. |
| context-assembly/tool_results::project; contracts::validate_plan | Plan and independently validate sequence resets, stable prefixes, bounded state and direct full-result references. |
| tool_output::dedup_policy, dedup_included, dedup_template | Resolve fresh host-owned policy, per-tool inclusion and mandatory live text without granting tool authority. |
| dedup_scope, record_dedup_origin | Capture existing call/run identity, scope and original hash before pending state is cleared; native names come from recorded calls. |
| dedup_prepare | Recover authorized journal originals, reject ambiguous/mismatched provenance, order observations, invoke linked/live planner, validate and persist through run fences. |
| dedup_summary_source, dedup_config_fingerprint | Expand original summary evidence and fence changed policy/template/inclusion at model dispatch. |
| pause_dedup | Preserve input; use a restorable component marker only for unavailable context-assembly, otherwise report projection/configuration error. |
| storage/activity::tool_result_origin, tool_result, tool_call_record | Resolve session-scoped immutable output and native call identity; reject ambiguity. |
| history/session storage and Base recall_adapter | Carry projection state between turns, mark compaction/history rebase and recall original output without reference mutation. |

The planner uses tool_results contract version 1, the webagent-context-assembly
library and existing webagent-context-service prepare envelope. Stable builds link
the library; live builds reuse the confined component generation. It adds no
per-ability connection. context-assembly remains context.prepare interface 2,
implementation 0.2.1. Its catalog declares service/implementation/contracts, Cargo
inputs and context-compaction config as build dependencies, no runtime component
dependencies, one concurrent request, 16 MiB framed request/response and 30 seconds.
Configuration, credentials, permissions, scope, encrypted persistence, original
reconstruction, final validation and run authority remain in the host.

## RPC execution metadata

Worker protocol 2 returns an outer version/id/result envelope. The worker checks
version before execution; the host checks version/correlation before accepting
result. Payload domain IDs, nested metadata and timestamps stay in returned
content. The protected build host keeps its existing flat protocol: host_reply
removes only its known outer routing id after correlation validation, never nested
job/release/domain IDs or arbitrary fields named id.

Existing model tool-call IDs still bind placeholders, results and recall. Existing
interaction sequences still identify durable private activity (interaction:{seq}).
RPC IDs correlate wire exchanges only; no new logical execution ID is introduced.
Version-1 execution Metadata stores rpc_calls for fixed peers tool-worker/build-host,
with at most 256 entries and nonempty control-free IDs up to 128 bytes.

execution::worker_reply/host_reply decode their explicit contracts. Host
outcome::capture creates task-local evidence for each independently polled
dispatch; rpc records verified exchanges and stage binds them to existing pending
call_id outside run.input. Outcome Deref/Display expose only content. Foreground,
ordered batch and durable background paths retain content/evidence separately.
Store::attach_execution_metadata validates and encrypts nullable
interactions.execution_metadata in the original result transaction, scoped to
session/run/step and fenced against immutable conflicts. Staging clears only
after commit. Recall reads the original interaction item. Authenticated private
activity exposes metadata beside payload.output; background metadata remains
session-fenced and withholds arguments/results. Public chat is not hydrated from
it, and old history is not rewritten/backfilled.

## Duplicate-result acceptance and historical pause

Initial feature publication b550e68d-325f-484f-b9d2-5f9e867fc943, native provenance
correction a27811a9-a4da-478b-97f4-477717f9cfa8 and settings correction
983a9d77-6eff-497a-a105-eb74c4fa5be5 / f2103ed36905882b6f2b1bd1fb5317b4a17ca686
have separate historical receipts. Do not rewrite their then-pending acceptance.

RPC/acceptance release 17ee81ba-d997-4a1c-9c10-69a7fc235470, commit
55a36e852ce19c72f5d9e9c477a8654ad513cc62, passed 987 workspace tests, five
existing ignored tests, both early profiles and optimized linked/live checks.
Under build-jobs/checks/tool-result-rpc-successor-20261019/:

- live-acceptance.json proves distinct routing evidence with identical stored
  content, duplicate references, fresh compaction promotion and original recall.
- live-lifecycle-acceptance.json proves three retained inputs, one eligible
  restoration, preserved manual pause/cancel and independent process replacement
  with unchanged core containers.
- release-commit.json and closure.json bind promoted bytes, commit, health,
  completed implementation and zero claims.
- post-compaction-reconciliation-20261019.json records the historical distinction.

Old run 0f607451-2374-5e8c-dcff-0eaea9295ccc in isolated session
bb6572e7-7a12-48fd-a51a-6143fa495434 paused with component_unavailable after one
compaction while component metadata reported available. Its cause remains
unconfirmed. It did not recur in fresh acceptance; it was not reproduced and
proven fixed, and there is no evidence assigning its disappearance to RPC ID
separation. The old fixture/history remain manually paused, with no component
restoration marker. Acceptance is complete; the unexplained historical anomaly
is not a currently reproduced defect. Do not resume it or reopen implementation
ownership for maintenance. [Session creation recovery](session-creation.md) is a
separately verified host fix and does not establish this pause's cause.
