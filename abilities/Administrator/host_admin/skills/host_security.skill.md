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
