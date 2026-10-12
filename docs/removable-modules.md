# Optional service modules

Automation and Outbound Notifications are independent executable packages. Their
source and distributable assets live under src/plugin_packages. The core binary
contains the scoped host protocol, not the scheduling or webhook engines.

Explicit administrator installation copies a fixed reviewed distribution into
the installation's data-root plugins/services folder (/state/plugins/services in
the containers, overridable by V3_MODULE_ROOT). Both API and loop share that
installation directory; app source permissions are never widened. Startup never
installs a missing package. Re-enable requires its last reviewed installation
identity; changed or unregistered directories require explicit reinstallation.
Installations can omit either service. App -> Modules controls installation;
package-owned pages appear in the main catalog only while available.

The host sends bounded protocol-1 JSON frames with server-authored caller,
agent, generation, settings, activation cursor and dispatch authority. Packages
return state transitions and work proposals. They never receive encryption keys,
raw session databases or arbitrary SQL access. Model context receives at most
16,000 bytes of UTF-8 source evidence with an explicit truncation marker; full
remembered state remains sealed. External event payloads and
remembered state become bounded untrusted evidence, never approval authority.

User-owned module_state documents are always sealed. Atomic occurrence receipts
commit with ordinary message/run/manager admission. Module generation, effective
access, policy, participation and quota fences apply to new and recovered work.
Package lifetime leases protect in-flight dispatch from administrative removal.

Disable and uninstall withdraw new work. Definitions and receipts are retained.
A new installation generation deactivates retained definitions; explicit resume
starts future recurring ticks or a fresh event cursor. Uncertain external effects
remain recorded and are never silently replayed.

## Public interfaces

- GET /api/v3/admin/modules lists reviewed distributions and availability.
- POST /api/v3/admin/modules/{id} accepts install, enable, disable or uninstall.
- GET /api/v3/modules/{id}/status returns bounded availability.
- POST /api/v3/modules/{id}/call accepts action, args and client_request_id.
- Package UI assets are authenticated fixed local files.
- Automation tools use the normal model capability and approval path.

The modules use revisioned operations and immutable request receipts. Destinations
are explicit; remind_me names its originating chat. Cron uses five fields and an
IANA timezone. Repeated DST times fire once; nonexistent times are skipped.
Missed recurring ticks during downtime are skipped. Overdue one-shots fire once
unless expired. Work serializes per definition; distinct event occurrences use
bounded queues. Retries default off and require reconciled prior effects.

Outbound webhooks use HMAC-SHA256 signing and stable delivery IDs. Receiver
addresses must be public HTTPS destinations, with DNS resolution pinned and
redirects disabled. External delivery is at-least-once; receivers deduplicate IDs.
Delivery claims commit before transport and are separate from agent execution.

Unported source/channel/scheduler/clone/device implementations are reported
unavailable. Their versioned adapter contracts do not establish live support.
The ability remains partial until the applicable authenticated acceptance passes.
