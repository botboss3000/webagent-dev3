# Host security: SSH access

How SSH into this server is restricted, how to check it, and how to undo it. Read this
before changing SSH settings or when someone cannot log in over SSH. Logging in at the
server's own keyboard and screen (console or desktop) is never affected by any of this.

## What is in place

One drop-in file, `/etc/ssh/sshd_config.d/10-webagent-hardening.conf`:

```
PasswordAuthentication no
# Only Tailscale (and the machine itself) may log in over SSH.
Match Address *,!100.64.0.0/10,!127.0.0.1,!::1
    DenyUsers *
```

- SSH accepts keys only; passwords are refused.
- SSH works only from Tailscale addresses (100.64.0.0/10) and from the machine itself.
  Logins from the home network or the internet by any other address are refused.
- Nothing else changes: no firewall rules, app ports, Cloudflare tunnel or local logins.

Who logs in over SSH: the operator workstation (T480) as `ar` (full access, to become
break-glass only) and as `webagent-ops` (release key, forced command `operator_shell.py`).

## Check it

```bash
sudo sshd -t                                                          # config valid
sudo sshd -T -C user=ar,host=x,addr=100.100.1.1 | grep -E '^(passwordauthentication|denyusers)'   # Tailscale: no denyusers line
sudo sshd -T -C user=ar,host=x,addr=192.168.1.50 | grep '^denyusers'  # others: denyusers *
```

## Undo it

From SSH over Tailscale or at the server's keyboard:

```bash
sudo rm /etc/ssh/sshd_config.d/10-webagent-hardening.conf && sudo systemctl reload ssh
```

To allow one more network instead, add `,!<network>/<bits>` to the `Match Address` line,
run `sudo sshd -t`, then `sudo systemctl reload ssh`.

## Changing it safely

Always run `sudo sshd -t` before a reload, reload (never restart) so existing sessions stay
open, and confirm a **new** SSH connection works before closing the old one. If a new login
fails, undo as above from the still-open session or from the console.

# Host approval passphrase

Boot loader and disk changes (paths in the host profile's `boot_disk_paths`, such as
`/etc/fstab`, `/etc/default/grub` and `/boot/*`), File Manager grants and root-equivalent
actions (`file_write`, `host_revert`, `broker_install`, `supervisor_update`,
`packages_upgrade`) need this passphrase on the approval card, and so does opening an
allow-all window from the page. `rotate_browser_login_token` always needs it from the page (or
`sudo buildctl host approve ID`): no allow-all window covers it. The root host helper checks it; the app never stores it. It
is separate from every login and `sudo` password, but it is root-equivalent: treat it like
the root password.

Set or change it at the server's own console (not over the app), as root:

```bash
sudo python3 -I /usr/local/lib/webagent-host/host_helper.py passphrase
```

It asks twice and needs at least 12 characters. Only a salted scrypt hash is kept, in
`/var/lib/webagent-host/passphrase.json` (root only). Five wrong entries within 15 minutes
lock boot and disk approvals for 15 minutes; failures are logged without the value. An empty
entry is refused without counting. The Host Admin page has one passphrase field per page load,
used by every card and the window form until the page is reloaded.
Until a passphrase is set, those requests cannot be approved from the page at all.

# Host operator route over SSH

`sudo buildctl host pending | approve ID | deny ID | allow HOURS [--session KEY]` reaches the
root host helper as root, never through the app, and needs no passphrase (sudo is the proof).
Decisions are logged as `host:<your account>`. `allow` opens a window for every requester
(`*`) unless `--session` names one; `allow 0` ends it. Use this route when the app may be
compromised. `/usr/local/sbin/buildctl` is a root-owned wrapper installed with the helper by
`supervisor_update`; it runs only the installed helper.

Who may open windows is the `window_openers` list in `/etc/webagent/host-profile.json`
(root-owned): `["app", "host"]` on the development server, `["host"]` in production (the
default when unset). Without `"app"`, the Host Admin page refuses to open windows, even with
the passphrase, and shows the SSH command and the time left instead. Ending a window is
always allowed from the page. Change the list with a root edit of the profile, then
`sudo systemctl restart webagent-host-helper.service`.

Who may approve root-equivalent, boot/disk and File Manager grant requests is the profile's
`approvers` list, set and changed the same way: `["app", "host"]` on the development server,
`["host"]` in production (the default when unset). Without `"app"`, the page refuses those
approvals even with the passphrase and each card shows `sudo buildctl host approve ID`
instead; it can still deny them and approve listed restarts.
