# Host Admin

Changes to the server itself go through the root host helper, never through shell or
file tools. You file a request with `host_admin`; an administrator approves or denies it
on the Host Admin page (https://dev.webagent.live/app?tab=host-admin). Nothing changes
until they do. This ability also owns `build_work`, the build broker tool (isolated source
tasks, checks and releases; see its tool skill). Both are development-server tools and
never ship in a production release. Editing app code, prompts and configuration stays in
Codebase Admin.

## Working with requests

- Read `host_admin` status first when the task depends on the host's state.
- One request per change, with the exact content: the card shows the user a diff or
  command, and that is what they approve.
- After filing, tell the user what is waiting and give the page link, then end your turn.
  A background notice wakes this chat when the request is decided and when it finishes.
  Never poll in a loop; `check` once when the user asks.
- Denied or expired means stop and ask; never file the same request again unasked.
- A finished `file_write` or `broker_install` can be undone once with `host_revert`; say so
  when it matters. Broker installs go through `broker_install`, never a shell.

For SSH access rules (keys only, Tailscale only) and how to undo them, read the
`host_security` skill.

## Limits the helper enforces

- Files only under the paths the host profile allows (by default `/etc/webagent/*`).
- Units and containers only from the host profile's lists.
- Boot loader and disk files need the host approval passphrase on approval, and an
  allow-all window never covers them.
- Root-equivalent actions (`file_write`, `host_revert`, `broker_install`, `supervisor_update`,
  `packages_upgrade`) also need the passphrase on approval, unless a live allow-all window
  covers the request. Opening a window needs it too, or is done by the host operator over SSH
  (`sudo buildctl host allow <hours>`); on hosts set to SSH-only windows (production) the page
  has no passphrase prompt for windows. Listed unit and container restarts need no passphrase.
  When you file a root-equivalent request, tell the user it needs the passphrase or a window.
- A request waits for a decision without a time limit; the user may decide it later.
