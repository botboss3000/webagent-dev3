# WebAgent v3 security specification

Status: living document, started 2026-11-20 from the app security audit.
Scope: the v3 app (`/home/ar/webagent-dev3/code`), its containers, the build broker,
the root host helper and the host `ar-server` (100.87.9.65). Production and v2 are out of scope.

Every change to v3 (code, abilities, config, containers, host) must satisfy the
requirements below. A change that breaks an **in force** requirement is a defect even if its
tests pass. A change in an area with a **target** requirement must not move further away from
it, and should say how it relates to it.

Companion documents:
- Coding rules: app skill `abilities/Administrator/codebase_admin/skills/security_practices.skill.md`
  (identity, per-user stores, ID binding, SQL, UI escaping, file confinement, outbound requests).
  This spec states *what* must hold; that skill states *how* to write code that keeps it.
- Audit evidence: operator notes kept on the T480 control project under `work/security-audit/`
  (`sec-host/plan.md`, `sec-browser/findings.md`, `sec-anon/anonymous-routes.md`,
  `sec-sweep/report.md`); not part of this checkout.

## 1. Status labels

| Label | Meaning |
| --- | --- |
| **In force** | Enforced on the server and covered by tests or a live check. Cited with file:line. |
| **Partial** | Enforced in part; the gap is named. |
| **Target** | Agreed requirement, not built yet. Names the work item that will build it. |

Line numbers are as of 2026-11-20 and drift; the function names are the stable reference.

## 2. Assets and threat model

### 2.1 What we protect, in order

1. **Users' data**: chats, files, workspaces, memories, provider and connection tokens
   (`state/user_data/<user>/`, `state/agent_data/<agent>/`, `/workspaces/<uuid>`), and the
   single encryption key `state/encryption.key`.
2. **The app's integrity**: source in `/home/ar/webagent-dev3/code`, release images, the build
   broker and its releases, ability manifests and config.
3. **The host**: root on `ar-server`, the Cloudflare tunnel token, SSH keys.
4. **Operator cost and availability**: model spend, run slots, disk, CPU.

### 2.2 Who we defend against

| Actor | Assumed capability |
| --- | --- |
| Anonymous internet visitor | Any request to `https://dev.webagent.live`; can guess IDs and labels. |
| Registered user, guest, embed visitor | Valid session for their own account; may be malicious. |
| Shared-chat member | Valid session plus membership in someone else's chat. |
| In-app agent | Runs tools on behalf of its owner; **assumed prompt-injectable** by any web page, email, file or message it reads. Treated as untrusted code with the owner's permissions. |
| Compromised app process | Code execution as the app user (uid 1100) or worker (uid 1103) inside `webagent-dev-v3`. |
| Operator workstation / SSH key holder | The T480 and anyone holding `ar`'s key. |

Principle: **an agent is never more trusted than the person it acts for, and a prompt-injected
agent must not be able to widen its own permissions, approve its own actions, or reach other
users.**

### 2.3 Trust boundaries

```text
Internet ─ Cloudflare ─ tunnel (host unit) ─ 127.0.0.1/100.87.9.65:8081
  └─ webagent-dev-v3 (API, uid 1100)  ── tool worker (uid 1103, Landlock + seccomp)
  └─ webagent-dev-v3-loop (uid 1100, /code read-only)
  └─ webagent-dev-v3-gate (uid 1102, network none)
  ── Unix sockets in runtime/ ──> build broker (root), host helper (root), board
Host: ar-server, Tailscale, ufw, sshd, systemd units
```

Each arrow must authenticate the caller (cookie + CSRF, bearer digest, or SO_PEERCRED uid) and
grant only what that caller needs.

## 3. Requirements

Each requirement has an ID for reference in commits and reviews.

### 3.1 Authentication and sessions (AUTH)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| AUTH-1 | Identity comes only from the auth middleware or a server-sealed token, never from request fields or headers. | In force | `src/api/auth.rs` `auth`; skill "Authorization" |
| AUTH-2 | Session cookie is HttpOnly, SameSite=Strict, Secure, Path=/. | In force | `crates/modules/accounts/src/protection.rs:46` |
| AUTH-3 | Every cookie-authenticated mutation checks the CSRF token and Origin. | In force | `auth.rs` `valid_csrf`, `valid_origin` |
| AUTH-4 | Passwords hashed with Argon2, minimum length 8. | In force | `accounts/src/authentication.rs` |
| AUTH-5 | Login is throttled per client and per account, and actually slows guessing. | In force | account checks serialized one per delay across all addresses (`throttling.rs` `login_gate`, `next_check`), commit `5b265e6` |
| AUTH-6 | Login response time must not reveal whether a username exists (always run Argon2). | In force | unknown and guest names verify against a dummy Argon2 hash (`authentication.rs` `dummy_hash`), commit `5b265e6`; live: unknown-user refusals take ~0.25 s |
| AUTH-7 | Bearer tokens are stored and compared as digests, length-capped, refused when unconfigured, and rate limited. | In force | messaging bearer `auth.rs:21-66`; browser-login bridge `bd30aa60` |
| AUTH-8 | A bearer that issues an admin session (diagnostics browser-login) refuses browser Origins, lasts ≤ 600 s, and is rate limited. | In force | `browser_login_api.rs`, commit `bd30aa60` |
| AUTH-9 | An embed (chat widget) token never carries admin power, whatever its owner's role, and only reaches its allowlisted paths for one agent. | Target | sweep M1/M2, `src/api/embed.rs:714` copies `_role` |
| AUTH-10 | Account creation from every path (register, guest-login, embed guest) counts against the daily new-account cap. | In force | embed guest cap, commit `bd30aa60` (sec-anon F1) |

### 3.2 Anonymous access (ANON)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| ANON-1 | Routes outside `auth` are an explicit, reviewed list (`src/api/mod.rs` public router). Each either returns no user data or binds the caller with its own secret (one-time code, digest-keyed secret, sealed token, PKCE). | In force | inventory in the sec-anon operator notes (2026-11-20) |
| ANON-2 | Build and broker status (task cards, job progress) requires an administrator sign-in. No public card page. | In force | `bd30aa60`, `acc1ef0` (page removed; links open the Builds page) |
| ANON-3 | Admin pages and APIs are enforced server-side by `admin_auth`, never only hidden in the UI. | In force | `src/api/mod.rs:281` |
| ANON-4 | The public wiki lists only published articles. | In force | checked by sec-anon (`src/api/wiki.rs`) |
| ANON-5 | One-time setup routes refuse once done. | In force | `setup-admin`, `authentication_api.rs:157-200` |
| ANON-6 | No anonymous client can exhaust a shared limit that blocks real users (embed challenges, harness pairing lockout). Limits are per address or per caller. | In force | ≤16 live embed challenges per address per agent (`embed.rs` `CHALLENGES_PER_CLIENT`); pairing wrong-code cap per address under a 10× shared ceiling (`request_limits.rs` `pairing_keys`), commit `5b265e6` |
| ANON-7 | Per-address limits apply to every request, including ones that reach the origin without Cloudflare's proof header. | In force | direct (untrusted) peers are limited by their own address; proxied requests without proof stay unknown (`request_limits.rs` `proven_client`), commit `5b265e6` |

### 3.3 Data isolation between users (DATA)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| DATA-1 | Every request runs against the caller's own encrypted store (`store.for_user`); handlers never read user data from the unscoped store. | In force | `auth.rs`; skill "Look up through the caller's own store" |
| DATA-2 | Another user's store is opened only through a membership gate (`shared_chat::resolve`, `run_app`). | In force | `chat/host/api/shared_chat.rs:33` |
| DATA-3 | A second ID in a path is bound to the path resource; "not found" and "not yours" return the same 404. | In force | skill rule; shared-chat `no_access()` |
| DATA-4 | Workspaces are addressed only by canonical UUID, symlinked workspace folders are refused, and file tools open paths with per-component O_NOFOLLOW. | In force | `worker/src/files/mod.rs:261-281`, `:6-7` |
| DATA-5 | No process that executes tools or agent code can read `state/` at the file-permission level. The worker is not in a group that owns `state/`. | Target | worker gid 1100 + `state/` 770 (Administrator-reach review, finding 1); Fable item "trust boundary" |
| DATA-6 | Provider and connection tokens never enter model context, logs, diagnostics, run records or exports. | In force (reviewed) | sweep "looks solid"; re-check on every new connector |
| DATA-7 | One encryption key per tenant on multi-tenant hosts, held outside the shared state volume. | Target | hosting (section 4) |
| DATA-8 | MCP and other connection-backed tools use the caller's own credentials, or require the owner's approval when a member or caller uses the owner's connection. | Target | sweep M6; Fable item "shared-chat rights" |

### 3.4 Agent tool permissions (TOOL)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| TOOL-1 | The tool decision is recomputed at execution time from one function shared with schema and discovery; a tool the model was not offered, or invented, cannot run on that basis. | In force | `engine/src/runtime/run.rs:1218-1229`, `policy.rs:245-276`, `access/src/authority.rs:323` |
| TOOL-2 | Permission layers combine by "most restrictive wins": tier, agent, level, ability, account active, membership, agent enabled, installed. | In force | `access/src/lib.rs:84` `meet`, `:359` |
| TOOL-3 | Administrator abilities run only when both the agent's owner and the caller are administrators, whatever any manifest, agent setting or tier list says. | In force | `abilities/src/ability_access.rs:78,101`, `policy.rs:257-262,279-288` |
| TOOL-4 | The admin-only set is a fixed list in code, checked against the `abilities/Administrator/` folder at startup; moving an ability out of the folder or reusing a tool name cannot make an admin tool non-admin. Powerful tools outside the group (`ssh_control`, `host_management`, sandbox) are classified deliberately. | Target | today admin-only = folder placement; Fable item "tool rules" |
| TOOL-5 | Registry missing ⇒ every ability is treated as administrator (fail closed). | In force | `access/src/lib.rs:147-151` |
| TOOL-6 | Tools of a discoverable (`on_request`) ability do not run until the ability is activated for the session, or the call is approved. | Target | today they run on first call: `abilities/src/catalog.rs:1818-1838` |
| TOOL-7 | An agent cannot change its own or another agent's tools, abilities, prompts, policies or approval settings without its human's approval at the time, and never through a standing "allow" setting. | Partial | Agent Management / Config Controller default `require_approval` and off on new agents (`agents/src/repository.rs:157-172`), but on for the Default Agent and settable to allow |
| TOOL-8 | An agent cannot enter Auto mode without a human approval. | In force | `run.rs:1282-1290` |
| TOOL-9 | Each tier and member level has a ceiling on execution mode (e.g. no Auto) and per-tool ceilings ("always ask", "off") that the user's own mode or settings cannot loosen. | Target | today Auto waives every "require approval" (`access/src/lib.rs:379`); Fable item "tool rules" |
| TOOL-10 | Powerful Core abilities (Agent Management, Config Controller, App Control, Automation, MCP, P2P, Model Switcher) are opt-in per tier, not open to free tier by default. | Target | `abilities/Core/<x>/<x>.json` `owner_tier` |
| TOOL-11 | Unattended runs (scheduler, automation, webhooks) keep approval requirements; a pending approval waits, it is never auto-granted. | In force (approval kept) | `for_schedule_approval`, `run.rs:1229`; sweep reviewed scheduler |
| TOOL-12 | Tools that send data out (MCP inspect, web fetch with arbitrary URLs, messaging) require approval for members and visitors. | In force | `mcp_inspect_server` require_approval for member and visitor, commit `642e192`; other send tools to review under TOOL-13 |
| TOOL-13 | Content from the web, browser, scraper, email and files is marked untrusted in the model context; after such content enters a run, write, send and settings tools require approval even where an owner set "allow". | Partial | attachments and incoming messages wrapped (`files/src/attachments.rs:24`, `chat/src/history.rs:39`); web/browser/scraper output not; Fable item "tool rules" |
| TOOL-14 | Admin shell commands and terminals run as uid 1103 under Landlock + seccomp: writes only to the workspace, no Unix sockets, TCP only to 80/443/preview/ephemeral, scrubbed environment. | In force | `sandbox.py:25-112`, `worker/src/.../commands.rs:246-260` |
| TOOL-15 | Workspace commands cannot reach internal addresses (loopback, Tailscale 100.64/10, link-local, metadata, Docker networks). | Partial | host fence `webagent-app-firewall.service` drops docker0 traffic to 10/8, 192.168/16, link-local, multicast (2026-11-20); Git remotes 443 only (`git/remote.rs`, commit `76b0fe6`). Tailscale 100.64/10 left open for SSH Control: Fable decides |
| TOOL-16 | Every server-side request to a user-influenced URL uses the shared outbound check (https, no credentials, global IPs only, DNS pinned, no redirects). | In force | skill "Tool execution and outbound requests"; sweep: web fetch SSRF guarded |

### 3.5 Agent browsers (BROWSER)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| BROWSER-1 | Agent browsers are not signed in to the WebAgent app by default; signed out, the app origin is refused. | In force | commit `233a9b6`, `config/browser/browser-pages.json` |
| BROWSER-2 | If sign-in is enabled, only pages listed in `config/browser/browser-pages.json` are reachable (default: Session Metrics for its own conversation). Settings, abilities, model, tools, policy, Administrator, Host Admin and Builds pages are never reachable. Checked before navigation, before clicks on links, and after every action (redirects and SPA navigation close the browser). | In force (tests only) | `233a9b6`; live check covered signed-out refusal only |
| BROWSER-3 | `browser-pages.json` is changed only by a reviewed source edit, never by the config composer or an agent. | In force | `233a9b6` |
| BROWSER-4 | The server refuses settings, permission and approval changes made with a browser-issued (agent) session, so in-page controls are inert even when a page is reachable. | Target | sec-browser open item 1; Fable item "tool rules" |
| BROWSER-5 | The Browser Extension (user's real Chrome) applies the same app-origin and page rules, not only a per-agent site list. | Target | sec-browser open item 2 |
| BROWSER-6 | Agent browsers cannot read input field values. | In force (observed) | sec-browser findings |
| BROWSER-7 | Agent-written UI previews or cards cannot run as first-party script in a user's app session. | Target | sweep M3 |
| BROWSER-8 | The bundled Chromium is kept within one stable release of upstream security fixes. | Target | Debian bookworm-security has newer Chromium, but the API image reuses a 09-30 package layer. Refreshing it changes the prepared runtime base, which needs broker dependency preparation under a `module:broker` claim (tried 2026-11-20, abandoned task `sec-supply-chain` to avoid blocking other broker work). Fable/broker owner |

### 3.6 Shared chats and members (SHARE)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| SHARE-1 | Every shared-chat route resolves access through one check that returns 404 to non-members. | In force | `shared_chat.rs:33` |
| SHARE-2 | Only the owner can widen access (invite, sharing settings); participants may only narrow or leave. | In force | `shared_chat.rs:92-96` |
| SHARE-3 | By default a member can send messages but cannot approve tool calls, change the session mode, or use the owner's connections and data without the owner's approval. | Target | today `can_control = write` (`shared_chat.rs:75`), so writers approve like the owner. User-accepted risk until built; Fable item "shared-chat rights" |
| SHARE-4 | The owner can grant a named member "full owner rights" explicitly; the grant is visible, revocable and recorded. | Target | as above |

### 3.7 Source, build and deployment integrity (BUILD)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| BUILD-1 | Source changes reach a release only through an isolated broker task, with unchanged-input checks, tests and a commit bound to the release. | In force | `build-service/`, AGENTS.md release rules |
| BUILD-2 | No app container mounts the Docker socket; no container is privileged; all use no-new-privileges, cap-drop ALL and a read-only root filesystem. | In force | container review 2026-11-20 |
| BUILD-3 | The API container mounts the source checkout read-only (as the loop container already does), runs its init without added capabilities (no SETUID/SETGID/CHOWN/FOWNER), and mounts only the sockets it needs, not all of `runtime/`. | Target | today `/code` rw, `/runtime` rw, tini root with 5 caps; Fable item "trust boundary" |
| BUILD-4 | `checkout_edit` and any other live-checkout write path is either removed or routed through a broker task. Nothing an agent runs writes the files the next release builds from. | Target | `worker/src/files/mod.rs:329-354,520-560` |
| BUILD-5 | Build inputs are attributable: the broker records which task and session authored each changed path, and refuses inputs changed outside a task. | Target | today it proves inputs did not change during the build, not who changed them |
| BUILD-6 | The broker and host helper authenticate every socket peer by SO_PEERCRED uid and grant per-uid operations. | In force | `board_host.py:343`, `boot_host.py:339-345`, `host_helper.py:587` |
| BUILD-7 | Broker installs are health-tested and automatically reverted on failure. | In force | host helper `broker_reverted` event, observed 2026-11-20 |
| BUILD-8 | Dependency vulnerability scanning runs and passes before releases; a failing scanner blocks releases or alerts the operator. | Partial | scanner repaired (`/usr/local/sbin/v3-supply-chain-scan` reads the release from the `/opt/v3/abilities` mount) and records failures in `supply-chain/latest.json`; first full scan 2026-11-20. Not yet a release gate or alert |
| BUILD-9 | Base images and toolchains are pinned by digest; the lockfile is honoured (`--locked`). | In force | all `FROM` images pinned by digest; Cargo `--locked` with lockfile checksums. Test step is not `--offline` (needs a prior `cargo fetch`): accepted |

### 3.8 Host actions and approvals (HOSTACT)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| HOSTACT-1 | Every host-changing action runs only after a recorded decision or inside a live allow window; reads run at once. | In force | `build-service/host_helper.py:1-8,492-512` |
| HOSTACT-2 | Decisions come only from the admin Host Admin page (admin cookie + CSRF + Origin), never from an agent's tool call. | In force | `administration/host/api/host_admin.rs:152-200`, admin router |
| HOSTACT-3 | Boot and disk changes also need the host approval passphrase, set at the console and checked only by the helper. | In force | `host_helper.py:505-534` |
| HOSTACT-4 | Allow windows never cover boot, disk or no-undo actions and are capped at 24 h. | In force | `host_helper.py` defaults, `host_admin.rs:176-200` |
| HOSTACT-5 | A compromised app process cannot approve its own root actions or open allow windows: decisions and windows need a factor the app process does not hold (host passphrase, operator SSH, or a signed confirmation). | Target | sweep H4: the helper trusts uid 1100 to decide; Fable item "trust boundary" |
| HOSTACT-6 | Allow windows are visible on the Host Admin page with requester, scope and expiry, and are logged. | In force (logged) | helper log `event: allow`; UI visibility to confirm |

### 3.9 Host and network (HOST)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| HOST-1 | Firewall default-deny inbound on IPv4 and IPv6; allowed: tailscale0, loopback, Docker bridges, established, UDP 41641. | In force | sec-host, 2026-11-20 (`ufw status` active) |
| HOST-2 | SSH: keys only, no root login, reachable only over Tailscale. | In force | `sshd -T`: `permitrootlogin no`; firewall restricts 22 |
| HOST-3 | Service metrics and admin ports listen on loopback or Tailscale only (cloudflared metrics 20241 local only). | In force | sec-host |
| HOST-4 | Unused network services are disabled (old build worker 8092, avahi). | In force | sec-host |
| HOST-5 | The tunnel token is a systemd credential, never on a command line or in a container. | In force | `webagent-development-tunnel.service --token-file %d/tunnel-token` |
| HOST-6 | `state/` is 770 `webagent:webagent`; `encryption.key` is not readable by `ar`. | In force | container review |
| HOST-7 | The operator account has no blanket passwordless root and is not in the `docker` group; routine release operations use a narrow account (`webagent-ops`) or specific sudo rules. | Target | `ar` has `NOPASSWD: ALL` and `docker`; Fable item |
| HOST-8 | Config files under `/etc/webagent/` are readable only by root and the accounts that need them. | Target | `build-service*.json` 644 (no secrets); `webagent-ops` runs `buildctl`, which reads it, so a plain chmod could break releases. With HOST-7 in the Fable plan |
| HOST-9 | Unattended security upgrades stay on. | In force | `unattended-upgrades` active |
| HOST-10 | Containers have CPU and memory limits so one runaway workload cannot starve the host. | In force | `docker update --cpus 4` on API and loop (memory 2 GiB / 768 MiB already set); replacements copy the running HostConfig (`host.py` `container_config`) |

### 3.10 Abuse and capacity (ABUSE)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| ABUSE-1 | Per-user and per-tier limits on concurrent runs, turns per run, model spend and storage, so open registration cannot take all run capacity. | Target | sweep H1 |
| ABUSE-2 | Request bodies capped (2 MB API, 16 KiB webhooks) and per-address rate limits applied. | In force | auth review |
| ABUSE-3 | Limits on open live-update streams per user. | In force | ≤32 open live views per user (`request_limits.rs` `stream_slot`, `chat.rs` `events`), commit `5b265e6` |

### 3.11 Web content and UI (UI)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| UI-1 | Strict CSP with `frame-ancestors 'none'`, `nosniff`, `no-store` on API responses. | In force | auth review |
| UI-2 | Markdown always through DOMPurify; plain text through `textContent`. | In force | skill "Rendering untrusted text" |
| UI-3 | User uploads are never served inline as active content (HTML, SVG) from the app origin. | Reviewed | sweep upload review; keep on every new download route |

### 3.12 File Manager outside the checkout (FILES)

| ID | Requirement | Status | Evidence / work item |
| --- | --- | --- | --- |
| FILES-1 | The File Manager reaches folders outside the checkout only through the host file service (`build-service/host_files.py`, socket `runtime/files/files.sock`). The service answers only uid 1100 (SO_PEERCRED), and every file operation needs a live `files_grant` for the signed-in administrator. A grant comes only from an approval with the host approval passphrase (on Host Admin or in the File Manager's own prompt; only the helper checks the passphrase), no allow window covers it, it lasts the time the approver sets (1 h by default, at most 24 h), and Host Admin revokes it without the passphrase. Each operation runs in a child process as the operator account without its `sudo` and `docker` groups, so it has that account's file permissions only; new files are `ar:ar 600`, new folders 700, edits keep owner and mode. App data (`state/`), sockets (`runtime/`), `/proc`, `/sys` and `/dev` are refused as well, and the checkout stays with the worker (`/live-checkout`). Every write, create, rename and delete is logged and shown on Host Admin. | In force | `host_files.py` `as_operator`, `host_helper.py` `files_grant`/`files_revoke`, `file_manager.rs` `host_files`; `tests/test_host_files.py`, `tests/test_host_helper.py`; commits `a7e5e5e`, `6ce6444` |
| FILES-2 | In-app agents never use the host file service, and only the File Manager may request its grants (an agent's or SSH request for `files_grant`/`terminal_grant` is refused); agent access outside the checkout would be a separate Administrator ability, off by default. | In force | `host_helper.py` `request`; sandboxed commands cannot open Unix sockets (TOOL-14); commit `1cb07c5` |
| FILES-3 | File Manager terminals need their own `terminal_grant` (same approval, time limit and revoke as FILES-1). Each is a login shell on a pseudo-terminal run as the operator account without its `sudo` and `docker` groups. At most 4 per administrator; they close when the grant ends, after 30 idle minutes or on request. Opening and closing are logged; input and output are not recorded. | In force | `host_files.py` `Terminal`, `terminal_open`; `TerminalTests`; commits `035ce55`, `6ce6444` |

The socket `runtime/files/files.sock` is reachable from the API container (rule 5.6): the File
Manager needs it to reach host folders and terminals, and it answers only uid 1100.

## 4. Multi-tenant and other hosted servers

A hosted deployment for other people must meet all **In force** requirements plus:

1. Ship signed, immutable images built elsewhere. No source mount, no build broker, no
   Codebase Admin, Host Admin or other Administrator abilities exposed to tenants.
2. One container, uid and volume set per tenant, or at minimum per-tenant directories that
   the app user of another tenant cannot open (DATA-5, DATA-7).
3. A per-tenant encryption key held in a KMS or systemd credential, not on the state volume.
4. Default-deny firewall, SSH only through a bastion or Tailscale, no passwordless-sudo or
   docker-group operator accounts (HOST-1, HOST-2, HOST-7).
5. Agent egress cannot reach the host's private addresses, metadata endpoints or other
   tenants' sockets (TOOL-15).
6. Per-tenant capacity and spend limits (ABUSE-1).

## 5. Rules for future work

1. **Cite the IDs.** A change that touches an area in section 3 names the requirement IDs it
   keeps, moves to In force, or affects, in its broker task summary or commit message.
2. **New routes** start inside `auth`. Adding a public route updates the ANON-1 inventory and
   names its own secret.
3. **New tools** declare a `default_policy` that is `require_approval` or `deny` for members and
   visitors unless the tool only reads the caller's own data. A tool that changes agents,
   permissions, settings, other users or the host is never `allow` for anyone but an
   administrator owner, and never `allow` when web content is in the run (TOOL-13 target).
4. **New abilities** with host, shell, SSH, source or cross-user reach go in the Administrator
   group (TOOL-3, TOOL-4).
5. **New connections and credentials** are stored encrypted in the owner's store, never logged,
   never placed in model context (DATA-6), and used only by their owner unless TOOL/DATA-8
   says otherwise.
6. **Containers, mounts, sockets and capabilities** only shrink. Adding a mount, capability or
   socket to an app container needs an entry here with the reason.
7. **Tests** for every access rule cover both sides with two real accounts (allowed and
   refused), kept in the owning module.
8. **Accepted risks** are recorded in section 6 with who accepted them and when, rather than
   silently left.
9. **Keep this file current.** When a Target ships, change its status to In force with the
   commit and test. When a requirement changes, record the decision and date in section 7.

## 6. Accepted risks

| Risk | Accepted by | Date | Until |
| --- | --- | --- | --- |
| Shared-chat writers can approve tools that use the owner's data and connections (SHARE-3). | User | 2026-11-20 | SHARE-3/4 ship |
| The operator may open a 24 h allow-all host window for all requesters (`session: "*"`) during active release work. | User ("I'm controlling it") | 2026-11-20 | HOSTACT-5 ships |
| SSH listens on all interfaces; the firewall restricts it to Tailscale. | sec-host recommendation | 2026-11-20 | — |
| With a File Manager grant (up to 24 h), the File Manager and its terminals can change anything the operator account can, including the live checkout without a broker task (FILES-1, FILES-3). | User | 2026-11-21 | — |

## 7. Work items and decision log

### 7.1 Open work, grouped by where it will be done

**Fable planning window (design-heavy):** BUILD-3/4/5, HOSTACT-5, DATA-5, HOST-7,
TOOL-4/6/7/9/10/12/13, BROWSER-4/5/7, SHARE-3/4, DATA-8, AUTH-9, ABUSE-1; then a review of the
shipped fixes (HOST-1..4, BROWSER-1..3, ANON-2, AUTH-8, AUTH-10).

**Fable planning window:** also BROWSER-8 (prepared-base refresh under a broker claim), HOST-8,
TOOL-15 Tailscale range, BUILD-8 as a release gate, sweep L1, L3 (Git token via fd), L5, L7, L8,
L10 (origin server timeouts), L11.

### 7.2 Decisions

- 2026-11-20: Audit baseline written. Host hardened (sec-host). Agent browser page limits
  shipped (`233a9b6`). Anonymous build status closed and the public Broker card page removed
  (`bd30aa60`, `acc1ef0`). User accepted the shared-chat member risk and the operator allow
  window (section 6). Tool code is compiled in; the open question is classification, not the
  tools' code (TOOL-4).
- 2026-11-20 (night): batches B–F. Shipped: `5b265e6` (AUTH-5/6, ANON-6/7, ABUSE-3, media slots
  per user), `76b0fe6` (Git 443 only), `642e192` (TOOL-12). Host: app container network fence
  (TOOL-15), CPU limits (HOST-10), supply-chain scanner repaired (BUILD-8), two inactive `muse-*`
  sudo files deleted. Deferred with reasons: BROWSER-8 (needs `module:broker`), HOST-8 (`webagent-ops`
  reads the config), L10 origin timeouts (origin is loopback/Tailscale only behind cloudflared),
  L3/L5/L7 (latent or mitigated; Fable). Assumptions to review: login queue allows two of the
  longest delays; 16 live embed challenges per address; 32 live views per user; media share is half
  the slots; Tailscale left reachable from containers; CPU limit 4 per app container.
- 2026-11-21: File Manager outside the checkout restored here after the B–F status update dropped
  it (FILES-1..3). File operations now run as the operator account instead of root (new files
  `ar:ar 600`); grant time is set at approval (default 1 h, maximum 24 h); terminals added with a
  separate grant.
