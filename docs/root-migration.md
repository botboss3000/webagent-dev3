# Independent V3 development root

V3 source and runtime storage live at `/home/ar/webagent-dev3`. V2 stays at
`/home/ar/webagent-dev2`. The V3 API, loop and metadata gate remain separate
containers with their existing names, users, limits, capabilities and networks.
The API exposes port 8081; the named development tunnel keeps its destination.

The writable `/live-checkout` mount contains only the independent V3 root.
Runtime code and UI remain pinned to frozen release snapshots. Shared V2 UI
assets enter the explicit build allowlist as read-only inputs; V3 containers do
not receive the writable V2 checkout. Never read session databases directly.

Build archives retain virtual `v3/` and `ui/` paths so historical candidate
hashes and receipts remain valid. The protected snapshot helper maps `v3/` to
the independent root and `ui/` to the selected V2 assets. Checkout promotion
removes the virtual `v3/` prefix only at the filesystem boundary.

Use `python3 -B /home/ar/webagent-dev3/build-service/buildctl.py`
for tasks. Edit isolated snapshots with checked unified diffs; finish queues
one tested development release. Run the installed protected bootstrap for
fixed-allowlist manual build snapshots. Keep runtime and workspace paths out
of source snapshots. Existing historical receipt paths are rebased during the
move; candidate archive bytes and input digests are preserved.
