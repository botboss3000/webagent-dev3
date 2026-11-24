//! Runtime acceptance tests using isolated encrypted storage and a local provider fixture.
use super::*;
use axum::body::Body;
use axum::extract::State as AxumState;
use futures_util::StreamExt;
use tempfile::TempDir;
use tower::ServiceExt;

// Tests live in the file of the module that owns the behaviour; add new tests there (or in the
// owning module crate), never back in this file. fixtures_tests.rs holds the shared helpers
// (the `_tests.rs` name keeps it out of the route-registration guard, like every test file).
mod fixtures_tests;
#[allow(unused_imports)]
pub(crate) use fixtures_tests::{native_catalog, copy_native, synthetic_codebase_admin, COMPONENT_FAILURE_FENCE, fixture_uninitialized, default_subscription_roster, fixture, set_task_gate, grant_fixture_levels, fixture_model, checkpoint_pre_tool, TEST_TIMEOUT, set_tool_permission, activate_codebase, conversation, queued, running, execute_fixture_journey, mode_entry, without_mode_entries, native_call_suffix, native_ability_frames, WARM_ITEMS, warm_stub, warm_failure, request_tools, ability_request, ability_auth, selection_fixture, BackgroundJourneyRequest, BACKGROUND_JOURNEY_STAGE, background_journey_request, background_journey_task, background_journey_final, background_journey_envelopes, background_execution_journey, copy_live_context_fixture, live_context_fixture_skill, live_context_default_stack_journey, visibility_fixture_base_access, visibility_fixture_ability_access, visibility_native_call, test_rpc_reply, late_single_control_reply, late_single_tool_case, guarded_turn};

mod runs_tests;
mod context_tests;
mod approvals_tests;
mod abilities_tests;
mod models_tests;
mod routes_tests;
mod publication_tests;
