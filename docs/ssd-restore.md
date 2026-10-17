# SSD swap: broker host support snapshot

The non-secret snapshot is config/deployment/ssd-host-support-20261017.json.
Use it with config/deployment/development.json and docs/restore-development.md.
It preserves host files outside this checkout, optional physical-console source,
its Python dependency versions, protected installation metadata, and a mapping
from every installed broker input to its tracked source hash.

No users, databases, encryption keys, access tokens, browser profiles, sockets,
task queues, review databases, build caches, Docker images or compiled boot
binaries are included. This restores configuration and supporting source, not
a running machine image. Rebuild the pinned images and boot authority through
the supported installation/release workflow. Do not restore old container IDs,
transient boot state, or a retention registry onto newly built images.

## Verify and materialize the saved files

From the fresh clone, run this as an ordinary user. It creates a new staging
directory and refuses to overwrite one. It does not write to /etc, /usr/local
or the live installation, and does not start any service.

```python
import hashlib, json, os
from pathlib import Path
snapshot = json.loads(Path("config/deployment/ssd-host-support-20261017.json").read_text())
stage = Path("/tmp/webagent-ssd-host-support")
stage.mkdir(mode=0o700)
for entry in snapshot["files"]:
    path = Path(entry["path"])
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("Invalid snapshot path")
    body = entry["content"].encode("utf-8")
    if hashlib.sha256(body).hexdigest() != entry["sha256"]:
        raise ValueError("Snapshot content mismatch")
    target = stage / str(path).lstrip("/")
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open("xb") as output:
        output.write(body)
    target.chmod(entry["mode"])
print("Verified and staged", len(snapshot["files"]), "files in", stage)
```

The snapshot records original numeric uid/gid and modes. Restore those through
reviewed root install operations on the replacement host; checkout ownership
alone is not sufficient for protected JSON, systemd units or executable helpers.

## Restoration order

1. Install a compatible Linux host, Docker/Buildx, Python, systemd, acl and
   Tailscale. The snapshot records the observed relevant package versions.
   Re-enroll Tailscale with separately supplied credentials. Verify the profile's
   fixed Tailscale address before applying network-bound installation settings.
2. Follow docs/restore-development.md to seed the saved live application and
   plugin settings and install the broker from the reviewed clone. The lifecycle
   installer must regenerate protected ownership, installed package identity
   and the broker unit. The saved /etc/webagent configuration is a comparison
   copy, not permission to overwrite an installed broker with stale paths.
3. Compare the regenerated package with installed_broker.files in the snapshot.
   Each installed source maps to a tracked source file and SHA256. Recreate
   installed.json through lifecycle installation; its saved copy is evidence.
   All observed installed source inputs matched tracked source when captured.
4. Restore the selected cleanup helper scripts and their timer/path/service units
   from the staged copy after reviewing the exact destinations and permissions.
   Preserve their task-aware retention behavior; never substitute blanket
   Docker prune. Start them only after protected broker configuration and the
   new retention service are healthy. Do not restore the old retention database.
5. Rebuild and start the application with the supported first-install workflow.
   Provision a fresh administrator and required connections; those live database
   records were deliberately excluded. Keep boot recovery disabled until a
   successful coordinated release has installed its attested launcher, stable
   executable and required boot-service artifact. Do not fabricate the old
   launcher receipt, adoption state or image/release IDs.
6. The saved boot systemd unit/drop-ins contain historical protected package
   paths. Point their ExecStart only to the newly verified protected boot_host.py
   after its hash matches the saved boot_host source mapping. Do not blindly
   copy an old /releases/HASH path onto a new installation. Reload systemd and
   enable the selected development services only after checking their targets.
7. Optional: restore /home/ar/webagent-boot source and the captured console
   wrappers. Recreate its .venv using the recorded console_python_packages;
   inspect the saved install.sh before running it. Historical console menus
   contain other environments: do not start or modify v2/production as part of
   this v3 restore. Getty autologin and profile hooks are saved for comparison;
   re-enable physical-console autologin only through an explicit host decision.
8. Restore the named Cloudflare tunnel and Git repository connections with
   separately supplied secrets, then verify readiness, broker status, a bounded
   build/check, plugin readiness and public app access.

## GitHub size and removal

This snapshot stores small text files and hash manifests, not large binary
archives. Each committed file stays below 100 MiB and the push below 2 GiB.
No Git LFS or release-asset download is needed for this backup.

After verifying the replacement host, remove the dated snapshot and this guide
through an ordinary reviewed commit if no longer useful. That removes them from
the current tree, not historical commits. Keeping this small non-secret snapshot
in history is intentional; do not rewrite shared history merely to remove it.
Large optional image/cache backups should use separately removable GitHub
Release assets (each under 2 GiB), not ordinary Git blobs.

Future agents should read both restore documents before the SSD swap and verify
that the GitHub main commit contains the backup commit. The saved source commit
inside the snapshot identifies the source at capture time; the later Git commit
adds the snapshot itself. No full clean-host restoration has yet been performed.
