# Host build service
All maintained host build and legacy source-workspace code lives here.
Administrator tools are protocol clients. Ordinary shell, browser, Git and live
file tools remain in the isolated worker. Agents receive neither root nor Docker
access. Docker-group membership is privileged host authority.

## Linux installation
Requires Docker/Buildx, Python 3, systemd, and the project's pinned Rust builder.
Run the reviewed lifecycle.py install --checkout-root /srv/webagent --instance
myapp --api-container myapp-api --loop-container myapp-loop --public-origin
https://dev.example.com as root. A missing default webagent-build service
account is created. Use --service-account for an existing Docker-enabled account.
Source ownership is resolved from the checkout independently of service identity.
Operators run buildctl as the configured service account (sudo -u ACCOUNT when
needed); Unix peer identity authenticates this operator channel.
Root-owned configuration under /etc/webagent/<instance>/ owns paths and identities.
CLI calls use WEBAGENT_BUILD_CONFIG or lifecycle --config. Writable configuration,
symlinks, and excluded targets are rejected.

Reviewed code installs under /var/lib/webagent/<instance>/build-service/releases/
<digest>/. The webagent-build-<instance>.service unit runs the protected supervisor.
The broker drops privileges; its helper supplies allowlisted snapshots and
creates only canonical session workspace directories with the fixed worker identity.
Use lifecycle validate, start, stop, status, drain and resume. A source release
cannot replace the installed service; operators install an exact reviewed tree
with --install-tree HASH while idle.

buildctl.py maintenance drain --task-id TASK persists a migration gate. Only
that exact local operator task may mutate source or finish a build while drained.
Other writes/checks/profiling are refused. Reads, receipts, cancellation and
lease controls remain available. maintenance resume reopens admission.
State stays under the configured state root. Sockets, PIDs and supervisor
diagnostics stay under the runtime root. Receipts and archive names are preserved.
Retain previous configuration, installed release and stopped application containers.
Reconcile uncertain effects before rollback. Shared UI inputs are bundled under
shared-ui; no v2 checkout or v2 application data is required. Fresh chat
configuration comes from the v3 template.

For a new instance, run deploy.py build, build-loop and build-gate with
WEBAGENT_BUILD_CONFIG, then start-gate, start and start-loop. The initial source
snapshot is indexed lazily by the service before the first workspace operation.
First-start configuration is seeded only when absent. Subsequent releases use
isolated broker tasks rather than direct deployment commands.

A dedicated account requires the Linux acl utility. The installer grants that
account source promotion rights only over source paths; it does not grant access
to application data. Operators retain access to newly promoted files through
directory default ACLs. Container UIDs remain fixed by the pinned images and
installation validation rejects incompatible identity values.

Run first-start deployment commands as root with the explicit protected
WEBAGENT_BUILD_CONFIG path. API bootstrap uses retained directory descriptors
to create app-owned data/config directories and seeds only absent defaults.
It rejects symlinks and preserves existing configuration contents. Ordinary
releases continue through the unprivileged broker and preserve runtime state.

An isolated local operator task changing only host.py, this README and
its test_build_service.py suite can publish after that exact host.py has been
installed while idle. The service verifies the protected installed bytes again,
freezes and indexes the source, verifies unchanged healthy application images
and served assets, then performs checked source promotion. Application, worker,
workspace executor, policy and other module changes always use the image build
workflow. This host-only path does not grant agents installation authority.
