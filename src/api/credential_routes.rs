//! Which routes the restricted credential kinds may call, decided by the identity middleware
//! from the matched route before any handler runs. Default deny: a chat-widget credential may
//! call only the routes listed with widget methods, and a read-only agent browser may read only
//! the GET routes marked readable. Person and test-persona sign-ins are decided by their role
//! (administrator routes check it), not by this table.
//!
//! Every route the app registers has exactly one row. `every_route_is_classified` reads the
//! router sources and fails the build when a route has no row (or a row has no route), so a new
//! route states which kinds may call it (build protocol A5).
use axum::http::Method;

/// One registered route pattern, exactly as `MatchedPath` reports it.
pub(crate) struct Route {
    pub(crate) path: &'static str,
    /// Methods a chat-widget credential may use ("" = refused for every method).
    widget: &'static str,
    /// A read-only agent browser may GET it.
    agent_browser: bool,
    /// Outside the signed-in layer: public, or checked by its own credential (messaging
    /// bearer, browser extension, webhook). No widget or agent-browser identity is built there.
    /// Read by the classification test, which checks it against where the route is mounted.
    #[cfg_attr(not(test), allow(dead_code))]
    public: bool,
}
const fn widget(path: &'static str, methods: &'static str) -> Route {
    Route {
        path,
        widget: methods,
        agent_browser: false,
        public: false,
    }
}
const fn both(path: &'static str, methods: &'static str) -> Route {
    Route {
        path,
        widget: methods,
        agent_browser: true,
        public: false,
    }
}
const fn read(path: &'static str) -> Route {
    Route {
        path,
        widget: "",
        agent_browser: true,
        public: false,
    }
}
const fn none(path: &'static str) -> Route {
    Route {
        path,
        widget: "",
        agent_browser: false,
        public: false,
    }
}
const fn public(path: &'static str) -> Route {
    Route {
        path,
        widget: "",
        agent_browser: false,
        public: true,
    }
}

/// The chat-widget allowlist comes first: what `ui/embed/` and the chat panel it mounts call
/// with the widget's bearer credential, nothing more. Never on it: administrator routes,
/// settings, permissions, approval settings, account and billing, File Manager, terminal,
/// Host Admin, builds, feedback (`POST /feedback` is for app sign-ins), session mode, model,
/// abilities, compaction, fork, undo, archive, rename and delete, invitations and messaging
/// routes. `embed::scope_request` then binds every session, run and approval to the widget's
/// agent.
pub(crate) const ROUTES: &[Route] = &[
    // The visitor's runs and the open conversation list (status poll every 3 s).
    both("/api/v3/user/status", "GET"),
    // List and start conversations; a new one is forced to the widget's agent.
    both("/api/v3/sessions", "GET POST"),
    // Conversation list search in the session dropdown.
    both("/api/v3/sessions/search", "GET"),
    // Open a conversation, follow it, and read its controls, usage and background state.
    both("/api/v3/sessions/{id}", "GET"),
    both("/api/v3/sessions/{id}/events", "GET"),
    both("/api/v3/sessions/{id}/controls", "GET"),
    both("/api/v3/sessions/{id}/usage", "GET"),
    both("/api/v3/sessions/{id}/background", "GET"),
    both("/api/v3/sessions/{id}/first-message", "GET"),
    widget("/api/v3/sessions/{id}/visit", "POST"),
    // Send, and the follow-up queue's delivery choices for the visitor's own messages.
    widget("/api/v3/sessions/{id}/messages", "POST"),
    widget("/api/v3/sessions/{id}/messages/{mid}/delivery", "PATCH"),
    widget("/api/v3/sessions/{id}/queue/resume", "POST"),
    // Stop everything in the conversation.
    widget("/api/v3/sessions/{id}/stop", "POST"),
    // Answer or dismiss a form the agent showed in the conversation.
    widget("/api/v3/sessions/{id}/components/{cid}/answer", "POST"),
    widget("/api/v3/sessions/{id}/components/{cid}/dismiss", "POST"),
    // Attachments up and down (the embed fetches them with its bearer).
    widget("/api/v3/sessions/{id}/attachments", "POST"),
    both("/api/v3/sessions/{id}/attachments/{aid}", "GET DELETE"),
    both("/api/v3/sessions/{id}/attachments/{aid}/content", "GET"),
    widget("/api/v3/sessions/{id}/attachments/{aid}/describe", "POST"),
    // Voice input.
    both("/api/v3/sessions/{id}/voice/capabilities", "GET"),
    widget("/api/v3/sessions/{id}/voice/audio", "POST"),
    // The conversation's recorded actions (read; resolving one is refused).
    both("/api/v3/sessions/{sid}/actions", "GET"),
    // The conversation's task list chip.
    both("/api/v3/sessions/{sid}/tasks", "GET POST"),
    // Shared-chat state, and asking a person for help.
    both("/api/v3/sessions/{sid}/sharing", "GET"),
    widget("/api/v3/sessions/{sid}/help", "POST DELETE"),
    // Cancel, recover, resume or retry the visitor's own run.
    widget("/api/v3/runs/{id}/{action}", "POST"),
    // Answer the visitor's own approval prompt (not approval settings).
    widget("/api/v3/approvals/{id}/decision", "POST"),
    // The bound agent's privacy mode before the first message (other agents refused).
    both("/api/v3/agents/{id}/access-summary", "GET"),
    // The Wiki reader opens an article the agent linked.
    both("/api/v3/wiki/articles/{slug}", "GET"),
    // Everything else, in path order.
    none("/admin/events"),
    none("/admin/events/stream"),
    none("/api/v3/abilities"),
    none("/api/v3/access-notices"),
    read("/api/v3/actions"),
    none("/api/v3/actions/{id}/resolve"),
    none("/api/v3/admin/ability-drift/dismiss"),
    none("/api/v3/admin/ability-drift"),
    none("/api/v3/admin/ability-drift/rebuild"),
    none("/api/v3/admin/access/accounts"),
    none("/api/v3/admin/access/accounts/{id}/profile"),
    none("/api/v3/admin/access/accounts/{id}"),
    none("/api/v3/admin/access/accounts/{id}/sessions"),
    none("/api/v3/admin/access/accounts/{id}/sessions/{session_id}"),
    none("/api/v3/admin/access/agents/{id}/ability-permissions"),
    none("/api/v3/admin/access/agents/{id}/status"),
    none("/api/v3/admin/access/agents/{id}/tool-callers"),
    none("/api/v3/admin/access/agents/{id}/tool-permissions"),
    none("/api/v3/admin/access/audit"),
    none("/api/v3/admin/access/memberships/{user_id}/{agent_id}"),
    none("/api/v3/admin/access/page-policy"),
    none("/api/v3/admin/access/registration"),
    none("/api/v3/admin/access/tiers"),
    none("/api/v3/admin/access/tiers/{id}"),
    none("/api/v3/admin/access/tiers/order"),
    none("/api/v3/admin/actions"),
    none("/api/v3/admin/actions/{user}/{id}/resolve"),
    none("/api/v3/admin/alerts"),
    none("/api/v3/admin/alerts/read"),
    none("/api/v3/admin/app-defaults/drafts"),
    none("/api/v3/admin/app-defaults/file"),
    none("/api/v3/admin/app-defaults/save"),
    none("/api/v3/admin/app-settings/encryption"),
    none("/api/v3/admin/builds"),
    none("/api/v3/admin/builds/{job_id}/cancel"),
    none("/api/v3/admin/builds/{job_id}/result"),
    none("/api/v3/admin/builds/leftovers/{id}/review"),
    none("/api/v3/admin/builds/scheduler"),
    none("/api/v3/admin/config-files"),
    none("/api/v3/admin/config-files/{*path}"),
    none("/api/v3/admin/configuration"),
    none("/api/v3/admin/configuration/{id}"),
    none("/api/v3/admin/context-compaction"),
    none("/api/v3/admin/diagnostics"),
    none("/api/v3/admin/diagnostics/issues"),
    none("/api/v3/admin/diagnostics/status"),
    none("/api/v3/admin/engine-components"),
    none("/api/v3/admin/engine-components/{id}"),
    none("/api/v3/admin/event-readers"),
    none("/api/v3/admin/event-readers/{id}"),
    none("/api/v3/admin/events"),
    none("/api/v3/admin/events/stream"),
    none("/api/v3/admin/guard/probe"),
    none("/api/v3/admin/host/allow"),
    none("/api/v3/admin/host/files/revoke"),
    none("/api/v3/admin/host/requests"),
    none("/api/v3/admin/host/requests/{id}/decision"),
    none("/api/v3/admin/loop-runtime"),
    public("/api/v3/admin/messaging/sessions/{id}"),
    public("/api/v3/admin/messaging/sessions/{id}/messages"),
    public("/api/v3/admin/messaging/sessions"),
    none("/api/v3/admin/models/catalog/entries/{entry_id}/credential"),
    none("/api/v3/admin/models/catalog"),
    none("/api/v3/admin/models/catalog/history"),
    none("/api/v3/admin/models/catalog/local-models"),
    none("/api/v3/admin/models/catalog/preview"),
    none("/api/v3/admin/models/catalog/publish"),
    none("/api/v3/admin/models/catalog/rollback"),
    none("/api/v3/admin/models/catalog/validate"),
    none("/api/v3/admin/modules"),
    none("/api/v3/admin/modules/{id}"),
    none("/api/v3/admin/production"),
    none("/api/v3/admin/production/{release_id}/decision"),
    none("/api/v3/admin/publication/status"),
    none("/api/v3/admin/services/boot_orphan_resume"),
    none("/api/v3/admin/supply-chain"),
    none("/api/v3/admin/system/dashboard"),
    none("/api/v3/admin/system/dashboard/{section}"),
    none("/api/v3/admin/system/{metric}"),
    none("/api/v3/admin/system/revision-shadow"),
    none("/api/v3/admin/system/summary"),
    none("/api/v3/admin/usage/models/analytics"),
    none("/api/v3/admin/usage/models/totals"),
    none("/api/v3/admin/usage/models/undo"),
    none("/api/v3/agents/{id}/abilities/{ability}/settings"),
    read("/api/v3/agents/{id}/abilities/{ability}/skill-content"),
    read("/api/v3/agents/{id}/abilities/{ability}/skills/{skill}/content"),
    none("/api/v3/agents/{id}/abilities/{ability}/skills/{skill}/visibility"),
    none("/api/v3/agents/{id}/abilities/{ability}/skill-visibility"),
    none("/api/v3/agents/{id}/abilities/{ability}/tools/{tool}/execution"),
    none("/api/v3/agents/{id}/abilities/{ability}/tools/{tool}/mode"),
    none("/api/v3/agents/{id}/abilities/{ability}/tools/{tool}/result-deduplication"),
    read("/api/v3/agents/{id}/abilities/{ability}/tools/{tool}/skill-content"),
    none("/api/v3/agents/{id}/abilities/{ability}/visibility"),
    read("/api/v3/agents/{id}/abilities/effective"),
    read("/api/v3/agents/{id}/abilities"),
    none("/api/v3/agents/{id}/ability-permissions"),
    read("/api/v3/agents/{id}/access-request"),
    none("/api/v3/agents/{id}/access-requests/{user}"),
    read("/api/v3/agents/{id}/actions"),
    read("/api/v3/agents/{id}/allowance"),
    none("/api/v3/agents/{id}/capability-ladder"),
    none("/api/v3/agents/{id}/capability-levels/{level}"),
    none("/api/v3/agents/{id}/chat-config/{document}"),
    read("/api/v3/agents/{id}/context-composition"),
    read("/api/v3/agents/{id}/context-composition/revision"),
    none("/api/v3/agents/{id}/embed"),
    none("/api/v3/agents/{id}/mcp/connections/{connection}"),
    none("/api/v3/agents/{id}/mcp"),
    none("/api/v3/agents/{id}/mcp/inspect"),
    none("/api/v3/agents/{id}/mcp/search"),
    read("/api/v3/agents/{id}/members"),
    none("/api/v3/agents/{id}/members/{user}/capability-level"),
    none("/api/v3/agents/{id}/model"),
    none("/api/v3/agents/{id}/repository-connections/{connection}"),
    none("/api/v3/agents/{id}/repository-connections"),
    read("/api/v3/agents/{id}/saved-scripts"),
    none("/api/v3/agents/{id}/saved-scripts/proposals/{proposal}"),
    none("/api/v3/agents/{id}/saved-scripts/scripts/{name}"),
    read("/api/v3/agents/{id}/shared-sessions"),
    read("/api/v3/agents/{id}/slots"),
    read("/api/v3/agents/{id}/slots/{name}"),
    none("/api/v3/agents/{id}/source-control/sites"),
    none("/api/v3/agents/{id}/source-control/sites/{site}"),
    read("/api/v3/agents/{id}/ui-elements"),
    none("/api/v3/agents/{id}/ui-elements/{scope}/{name}"),
    public("/api/v3/auth/access-mode"),
    none("/api/v3/auth/change-password"),
    public("/api/v3/auth/guest-login"),
    public("/api/v3/auth/login"),
    none("/api/v3/auth/logout"),
    read("/api/v3/auth/me"),
    none("/api/v3/auth/profile"),
    public("/api/v3/auth/register"),
    read("/api/v3/auth/session-policy"),
    none("/api/v3/auth/sessions"),
    none("/api/v3/auth/sessions/{id}"),
    public("/api/v3/auth/setup-admin"),
    public("/api/v3/auth/setup-status"),
    read("/api/v3/auth/ui-config"),
    public("/api/v3/browser/ack"),
    none("/api/v3/browser/devices"),
    none("/api/v3/browser/devices/{id}"),
    none("/api/v3/browser/link-code"),
    public("/api/v3/browser/link"),
    public("/api/v3/browser/next"),
    public("/api/v3/browser/result"),
    none("/api/v3/browser/sites"),
    none("/api/v3/browser/sites/{id}"),
    public("/api/v3/browser/unlink"),
    none("/api/v3/builds/tasks/{task_id}"),
    none("/api/v3/chat-invitations/redeem"),
    none("/api/v3/config/apply"),
    none("/api/v3/config/propose"),
    read("/api/v3/config/settings"),
    none("/api/v3/control"),
    none("/api/v3/core/vault/{sid}/{name}"),
    public("/api/v3/core/webhook-receive/{id}"),
    none("/api/v3/core/webhooks/{id}/setup"),
    read("/api/v3/diagnostics/background/config"),
    none("/api/v3/diagnostics/background/events"),
    read("/api/v3/diagnostics/ui-trace/config"),
    none("/api/v3/diagnostics/ui-trace/events"),
    none("/api/v3/embed/authorize"),
    none("/api/v3/embed/challenges/{id}"),
    none("/api/v3/embed/credentials/{id}"),
    public("/api/v3/embed/{id}/challenge"),
    public("/api/v3/embed/{id}/descriptor"),
    public("/api/v3/embed/{id}/guest"),
    public("/api/v3/embed/redeem"),
    none("/api/v3/feedback"),
    none("/api/v3/file-manager"),
    public("/api/v3/harness-bridge/ack"),
    public("/api/v3/harness-bridge/next"),
    public("/api/v3/harness-bridge/pair"),
    public("/api/v3/harness-bridge/progress"),
    public("/api/v3/harness-bridge/reply"),
    none("/api/v3/messages"),
    none("/api/v3/models/agents/{agent}/main-notes/proposals/{id}"),
    none("/api/v3/models/agents/{agent}/main-notes/topics/{id}/delete"),
    read("/api/v3/models/agents"),
    none("/api/v3/models/agents/{id}/adoptions"),
    none("/api/v3/models/agents/{id}/credentials"),
    none("/api/v3/models/agents/{id}/credentials/load"),
    none("/api/v3/models/agents/{id}"),
    read("/api/v3/models/agents/{id}/identity"),
    read("/api/v3/models/agents/{id}/main-chat"),
    read("/api/v3/models/agents/{id}/main-notes"),
    read("/api/v3/models/agents/{id}/members"),
    none("/api/v3/models/agents/{id}/remove"),
    none("/api/v3/models/agents/{id}/unarchive"),
    none("/api/v3/models/bridge/{provider}/clients/{client}"),
    none("/api/v3/models/bridge/{provider}/clients"),
    none("/api/v3/models/bridge/{provider}/pairing"),
    read("/api/v3/models/catalog"),
    none("/api/v3/models/connections/authorization/pending"),
    none("/api/v3/models/connections/{id}/api-key"),
    none("/api/v3/models/connections/{id}/authorization"),
    none("/api/v3/models/connections/{id}/authorization/reset"),
    none("/api/v3/models/connections/{id}"),
    none("/api/v3/models/connections/{id}/models"),
    none("/api/v3/models/connections/subscription/reset"),
    none("/api/v3/models/discovery"),
    none("/api/v3/models/me/catalog/default"),
    none("/api/v3/models/me/catalog/entries/{entry}"),
    read("/api/v3/models/me/catalog"),
    read("/api/v3/models/providers"),
    read("/api/v3/modules/{id}/assets/{asset}"),
    none("/api/v3/modules/{id}/call"),
    read("/api/v3/modules/{id}/status"),
    read("/api/v3/notices"),
    none("/api/v3/notices/read"),
    public("/api/v3/pages/catalog"),
    none("/api/v3/provider/login"),
    none("/api/v3/provider/logout"),
    none("/api/v3/sessions/{id}/abilities/{ability_id}"),
    read("/api/v3/sessions/{id}/abilities"),
    read("/api/v3/sessions/{id}/activity"),
    none("/api/v3/sessions/{id}/archive"),
    none("/api/v3/sessions/{id}/compact"),
    none("/api/v3/sessions/{id}/fork"),
    read("/api/v3/sessions/{id}/memories"),
    none("/api/v3/sessions/{id}/memories/{mid}"),
    read("/api/v3/sessions/{id}/messaging-routes"),
    none("/api/v3/sessions/{id}/messaging-routes/{target}"),
    read("/api/v3/sessions/{id}/mode"),
    none("/api/v3/sessions/{id}/model"),
    none("/api/v3/sessions/{id}/runs/{run}/context-remedy"),
    none("/api/v3/sessions/{id}/source-review"),
    none("/api/v3/sessions/{id}/state-inspect"),
    none("/api/v3/sessions/{id}/unarchive"),
    none("/api/v3/sessions/{id}/undo"),
    none("/api/v3/sessions/{id}/voice/reply"),
    none("/api/v3/sessions/{sid}/actions/{id}/resolve"),
    none("/api/v3/sessions/{sid}/administrator/artifacts/{id}"),
    none("/api/v3/sessions/{sid}/administrator/notices"),
    none("/api/v3/sessions/{sid}/administrator/notices/{id}/review"),
    read("/api/v3/sessions/{sid}/background-tasks/{task_id}"),
    none("/api/v3/sessions/{sid}/build-tasks/clear"),
    read("/api/v3/sessions/{sid}/build-tasks"),
    none("/api/v3/sessions/{sid}/build-tasks/{task_id}/cancel"),
    none("/api/v3/sessions/{sid}/build-tasks/{task_id}/clear"),
    read("/api/v3/sessions/{sid}/build-tasks/{task_id}"),
    none("/api/v3/sessions/{sid}/build-tasks/{task_id}/jobs/{job_id}/stop"),
    none("/api/v3/sessions/{sid}/invitations/{id}"),
    none("/api/v3/sessions/{sid}/invitations"),
    read("/api/v3/sessions/{sid}/issues"),
    none("/api/v3/sessions/{sid}/join"),
    none("/api/v3/sessions/{sid}/operator-replies"),
    none("/api/v3/sessions/{sid}/participation"),
    read("/api/v3/sessions/{sid}/runs/{rid}/diagnostics/context/{attempt_id}"),
    read("/api/v3/sessions/{sid}/runs/{rid}/diagnostics"),
    read("/api/v3/sessions/{sid}/runs/{rid}/logs"),
    read("/api/v3/sessions/{sid}/runs/{rid}/progress"),
    read("/api/v3/sessions/{sid}/ui-elements/{scope}/{name}/frame"),
    read("/api/v3/sessions/{sid}/ui-elements/{scope}/{name}"),
    none("/api/v3/sessions/{sid}/ui-preview"),
    read("/api/v3/sessions/{sid}/ui-preview/{variant}"),
    read("/api/v3/sessions/stats"),
    none("/api/v3/settings"),
    none("/api/v3/status"),
    read("/api/v3/storage/cleanup-policy"),
    none("/api/v3/storage/cleanup"),
    none("/api/v3/storage/compact"),
    read("/api/v3/storage"),
    read("/api/v3/storage/sessions"),
    read("/api/v3/ui-preview"),
    read("/api/v3/usage/models/analytics"),
    none("/api/v3/usage/models/reset"),
    read("/api/v3/usage/models/totals"),
    none("/api/v3/usage/models/undo"),
    read("/api/v3/user/agent-page-favorites"),
    read("/api/v3/user/app-page-favorites"),
    read("/api/v3/voice/samples/{voice}"),
    none("/api/v3/wiki/articles"),
    read("/api/v3/wiki/articles/{slug}/backlinks"),
    read("/api/v3/wiki/articles/{slug}/history"),
    read("/api/v3/wiki"),
    read("/api/v3/wiki/revisions/{id}"),
    read("/api/v3/wiki/search"),
    none("/api/v3/workflow/policy"),
    public("/app"),
    public("/config/ui/{*path}"),
    public("/embed/{id}"),
    public("/"),
    public("/health/live"),
    public("/health/ready"),
    public("/robots.txt"),
    public("/sitemap.xml"),
    public("/wiki"),
    public("/wiki/{slug}"),
];

impl Route {
    /// The methods a chat-widget credential may use on this route.
    pub(crate) fn widget_methods(&self) -> impl Iterator<Item = &'static str> {
        self.widget.split_whitespace()
    }
}

fn find(matched: &str) -> Option<&'static Route> {
    ROUTES.iter().find(|route| route.path == matched)
}

/// Whether a chat-widget credential may call `method` on the matched route. An unmatched or
/// unknown route is refused.
pub(crate) fn widget_may(method: &Method, matched: Option<&str>) -> bool {
    let method = if method == Method::HEAD {
        "GET"
    } else {
        method.as_str()
    };
    matched
        .and_then(find)
        .is_some_and(|route| route.widget_methods().any(|allowed| allowed == method))
}

/// Whether a read-only agent browser may read the matched route (its method rule refuses
/// every write first). An unmatched or unknown route is refused.
pub(crate) fn agent_browser_may_read(matched: Option<&str>) -> bool {
    matched
        .and_then(find)
        .is_some_and(|route| route.agent_browser)
}

#[cfg(test)]
#[path = "credential_routes_tests.rs"]
pub(crate) mod tests;
