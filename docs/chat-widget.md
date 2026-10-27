# V3 chat widget

The widget mounts the actual V3 chat panel. `ui/chat/core/panel.js` owns chat behavior in docked, floating and iframe views; `ui/chat/widget/window.js` owns only window geometry and launchers. Float/Dock moves the mounted native panel, retaining its stream, draft and attachments. Minimize and Close hide it without cancelling a run. Stop remains explicit.

## Configure

Presentation reads the live `ui/chat/chat.json`: merge `chat_common` with `chat_widget`. Existing widget formatting stays supported. Add these optional fields under `chat_widget` (shown defaults):

```json
{
  "window": {"width":440,"height":620,"min_width":300,"min_height":320,"movable":true,"resizable":true,"radius":16,"background":"","theme":""},
  "launcher": {"mode":"button","shape":"circle","position":"right","label":"Chat","placeholder":"Type a message"}
}
```

Launcher modes are `button` and `pill`; shapes are `circle`, `rounded-square`, and `pill`. Position accepts `left`/`right` or `bottom-left`, `bottom-right`, `top-left`, `top-right`. Pill click expands the input; submission opens the same conversation and sends through normal admission. Failed sends retain the text. Configure header `rows` with `session_dropdown` and `new_session` to expose conversation controls; `minimize` and `close` are supported in the widget header. The shell always has accessible window controls. Legacy launcher corner actions remain unavailable rather than granting new operations. Dimensions are CSS pixels, clamped to viewport and minimums. `window.radius` sets the outer corner radius in pixels (0–64). `window.background` is a CSS colour for the window and the chat surface, which derives its panel tints from it; `window.theme` (`light` or `dark`) fixes the text theme to suit that colour instead of following the visitor's stored preference. Empty or invalid values keep the defaults. Arrow keys on the window grip move it; Shift+Arrow resizes it.

On Agents → Identity → Website chat widget, explicitly enable embedding and list exact allowed origins, such as `https://example.com`. HTTP is accepted only for localhost development. Guest chat additionally requires the agent’s explicit “Allow guest chat” setting and the installation’s existing guest admission mode. New guest accounts receive only this agent’s visitor membership; no abilities or tool permissions are added. Saving settings uses revision fencing and invalidates existing embed credentials. This authorizes a website location, never an agent ability.

## Install and control

```html
<script src="https://dev.webagent.live/v3-ui/embed/loader.js" data-agent="AUTHORIZED_AGENT_ID" async></script>
```

The loader isolates window/launcher CSS in a Shadow DOM and loads `/embed/AGENT_ID?origin=HOST_ORIGIN`. Only this dedicated route permits configured framing; native `/app` remains unframeable. The public descriptor exposes chat presentation only. Host CSP must permit the WebAgent script, module, stylesheet, descriptor connection and iframe.

`window.WebAgentWidget` exposes asynchronous `open()`, `minimize()`, `close()`, `setDraft(text)`, `send(text)`, `selectSession(sessionId)`, `newSession()`, `stop()`, `continue()`, `approve(approvalId, decision)` and `recover(runId, decision, output)`, plus `on(event, listener)` and `destroy()`. Approval decisions are `approve` or `deny`; recovery uses the existing V3 decisions and optional known output. Only the selected conversation's pending approval or recovery run can be acted upon. Run events include IDs/statuses so the host can identify these controls; approval events include approval IDs and recovery eligibility but no tool arguments or raw diagnostics. All actions are policy checked.

The iframe bridge uses namespace `webagent.widget`, version 1, a fresh iframe channel, verified origins/windows, command IDs and acknowledgments. Duplicate IDs with changed input fail; identical commands within the bounded receipt cache reuse the result. A timeout is an uncertain UI result: inspect the session before retrying a mutation. Remounts withdraw the previous channel. Events include `ready`, `open`, `minimize`, `close`, `session`, `runs`, `sent`, `authentication`, and `error`; they contain no credentials or private diagnostics.

## Authentication and limitations

Guests follow existing app admission and agent-access policy. Restricted installations require sign-in. The embed has no sign-in strip: guests are admitted on their first message, and visitors with an account type `/login` to open the sign-in panel. Signed-in visitors use a first-party popup, review the verified agent/website, and click Connect. A five-minute S256 challenge is redeemed once by the originating iframe. Embed credentials expire within one hour and stay in iframe memory; blocked third-party cookies do not prevent chat. Reloading or expiration requires reconnection. Guest identity/history is preserved while this iframe lives; guest-to-account history migration and website identity federation are not included.

Embed credentials are scoped to one account, agent and configured origin, support caller-owned chat endpoints, and cannot reach unrelated administration APIs. Changes to the account, source sign-in, embed configuration or live agent access revoke them; active streams recheck authorization periodically. Native cookies retain Strict SameSite and CSRF behavior. Authenticated attachment fetches become iframe-local blob URLs rather than putting credentials in asset URLs. Signed-in browser permission prompts still govern clipboard, microphone and media functions.

## Verify

Run `node --experimental-vm-modules --test ui/chat/widget/widget.test.cjs ui/chat/core/transcript-recovery.test.cjs ui/chat/core/transcript-anchor.test.cjs` against unchanged task inputs. Rust security/router tests live in `api::embed::tests`; the broker runs the pinned locked Rust suite before a full release. Browser acceptance includes surface switching during a run, both themes, narrow viewport, resizing/moving, close/restore, both launcher modes, first-party sign-in, scoped session controls, forbidden-origin framing, hidden read acknowledgments and exactly one persisted final reply.
