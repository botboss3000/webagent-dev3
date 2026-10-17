# Recreate the development installation from Git

This profile is a reviewed snapshot, not a continuously synchronized copy of
runtime state. `config/deployment/development.json` records non-secret live app
settings, broker installation policy and installed MCP, Diagnostics and
Optimization package configuration. Their executable source remains in
`src/plugin_packages/`; binaries are rebuilt from the matching checkout.
The ordinary `config/*.json` files remain general defaults.

## Boundaries

Use only a fresh, offline v3 checkout on the replacement development server.
Do not run this bootstrap against an existing deployment or overwrite its data.
Keep the same checkout path, account and Tailscale address recorded in the
profile, or review a new host-specific profile before installation.
The profile retains current access-policy tiers; it does not recreate users,
agent assignments or encrypted provider connections stored in databases.

Credentials, encryption keys, databases, sessions, workspaces, task receipts,
release history, package logs and temporary diagnostic capture sessions are
excluded. Restoring existing application data requires a consistent separate
backup together with its original encryption key. For a genuinely new app,
provision new users and connections through supported administration.
Cloudflare tunnel credentials and Tailscale enrollment are separate secrets.

## First-start configuration

Install Docker/Buildx, Python 3, systemd and the Linux acl utility. Create the
non-root source/service account declared by the profile and grant the required
Docker access. Restore network connectivity first. Docker-group membership is
privileged host authority. Clone the intended reviewed commit into the recorded
checkout path, owned by that account. Do not use recursive submodule cloning:
the legacy v2_ref gitlink lacks a .gitmodules mapping and is not a v3 build input.
Shared UI inputs are tracked in build-service/shared-ui.

The following root-only first-install procedure creates app-owned configuration
and the root-owned broker input. It refuses existing data, plugin packages or
protected configuration. Run from the clone with no v3 containers or broker
service running. It never starts services, builds, deploys or modifies tracked
source. Its symlink checks complement the offline/no-other-writers requirement.

```python
# Run with: sudo python3 -  (paste this block on standard input)
import json, os, stat
from pathlib import Path
root = Path.cwd()
profile = json.loads((root / "config/deployment/development.json").read_text())
assert profile["schema_version"] == 1
broker = profile["broker"]
assert os.geteuid() == 0
assert str(root) == broker["checkout_root"]
protected = Path("/etc/webagent") / broker["instance"] / "build-service.json"
targets = [root / "data", protected]
targets += [root / "plugins" / name for name in profile["plugins"]]
for target in targets:
    if target.exists() or target.is_symlink():
        raise RuntimeError("Fresh-install target already exists: " + str(target))
def directory(path, uid, gid, mode):
    if path.is_symlink():
        raise RuntimeError("Symlink directory rejected")
    if not path.exists():
        directory(path.parent, uid, gid, mode)
        path.mkdir(mode=mode)
        os.chown(path, uid, gid)
        path.chmod(mode)
    if not stat.S_ISDIR(path.lstat().st_mode):
        raise RuntimeError("Regular directory required")
def write(path, value, uid, gid):
    directory(path.parent, uid, gid, 0o770 if uid else 0o755)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        os.fchown(stream.fileno(), uid, gid)
        os.fchmod(stream.fileno(), 0o660 if uid else 0o600)
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
for name, value in profile["application"].items():
    assert name in {"chat.json", "prompts.json", "app-settings.json",
                    "loop-runtime.json", "engine-components.json",
                    "context-compaction.json", "tool-output-retention.json",
                    "access-policy.json", "gates/message_visibility_v1.json",
                    "gates/closing_status_v1.json"}
    write(root / "data/config" / name, value, broker["app_uid"], broker["app_gid"])
write(protected, broker, 0, 0)
print("Configuration seeded; no service started")
```

Do not retry a partially completed bootstrap blindly. Inspect the created files
and reconcile the interrupted operation first. The committed profile is desired
configuration, not permission for agents to edit root-owned live configuration.
For an existing installation use domain revision-fenced app APIs and protected
lifecycle reconfiguration during an idle task-owned drain.

## Build and start through the supported installation workflow

1. Review the protected JSON and run the checkout's lifecycle installer as root:
   `WEBAGENT_BUILD_CONFIG=/etc/webagent/development/build-service.json python3 -B build-service/lifecycle.py install --config /etc/webagent/development/build-service.json --checkout-root /home/ar/webagent-dev3 --service-account ar`.
   The installer computes service_dir from exact installed source; the profile's
   releases/bootstrap value is deliberately not an old machine's package hash.
   It generates the protected broker service and validates identities/paths.
2. Run lifecycle validate and start with that same --config. Confirm broker health.
   Pull the exact pinned Rust and BuildKit images declared by executor.py.
   Rebuild the frontend-test image from build-service/ui-test.Dockerfile, inspect
   its exact image ID, and adopt that ID through protected scheduler
   reconfiguration during an idle task-owned drain. The recorded old ID is an
   identity reference, not a pullable registry name or authority to accept a
   different image. Retain normal resource admission and ownership enforcement.
3. Use the documented first-install deploy.py build, build-loop and build-gate
   commands with the explicit protected configuration, as root. The build helper
   supplies the virtual v3/ and ui/ archive namespaces; docker build . and the
   historical compose.yaml are not substitutes. Initial downloads require access
   to the pinned registries, crates and signed Debian repositories. Caches can be
   recreated; preserve signature/checksum verification. Do not request offline
   Cargo checks until the registry cache has been populated.
4. Explicitly install each package listed below from the freshly built API image,
   before starting the application. Extract only the named executable from
   /usr/local/bin into a private temporary directory using a disposable container
   created from the exact new API image ID. Verify its hash against that image.
   Assemble the package in a private staging directory: copy its tracked
   source_assets from the clone, replace configuration files with profile values,
   add the matching executable, set administrator ownership, directories 0755,
   executable 0755 and assets 0644, then rename the complete directory into its
   absent plugins/NAME destination. Keep Diagnostics configuration writable by
   app UID/GID 1100 (0660); do not restore logs.db or capture-session grants.
   Remove the disposable extraction container after reconciliation.
5. Run deploy.py start-gate, start and start-loop as root, with the explicit
   WEBAGENT_BUILD_CONFIG. Check all three containers, readiness, actual plugin
   readiness and a persisted authenticated app journey. Restore the named
   Cloudflare tunnel using separately supplied credentials and verify the public
   origin. Register boot supervision through the separately documented
   docs/boot-supervisor.md workflow; generated systemd units are host outputs,
   not backups of task state.

| Package | Tracked source assets | Image executable |
| --- | --- | --- |
| mcp | src/plugin_packages/mcp/plugin.json, mcp.json | webagent-mcp |
| diagnostics | src/plugin_packages/diagnostics/plugin.json, diagnostics.json | webagent-diagnostics |
| optimization | src/plugin_packages/optimization/plugin.json, optimization.json, optimization.skill.md, measure.py, workload.py | webagent-optimization |

Relative asset names after the first comma use the same package directory.
The machine-readable source_assets list in the profile is authoritative for
this restore inventory. Installed executable packages are not automatically
recreated merely by restarting the app.

## Keep the snapshot current

After a deliberate configuration change, export only the named app/plugin
configuration and protected broker JSON into an isolated broker task. Inspect
all string values for credentials and connection secrets, remove temporary
capture authorizations, normalize service_dir to releases/bootstrap, validate
against the current configuration contracts, and publish/commit the reviewed
profile. Publishing this profile does not overwrite existing runtime settings.
Preserve secrets in a separate secure backup and report when the Git remote
does not yet contain the restore commit.
