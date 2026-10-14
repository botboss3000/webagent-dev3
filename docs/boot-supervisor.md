# Event-driven boot supervision
The console sleeps on a black screen until keyboard input. Container lifecycle and
Docker health events remain independent of presentation. Docker restart policies
handle process exits; the host handles startup and bounded unhealthy recovery.

The version-1 boot.plan contract contains at most 32 metadata-only service positions.
webagent-boot-supervisor implements selection/deadlines; webagent-boot-service uses
the reusable confined pipe wrapper. The stable linked build calls that library
directly through --boot-plan before application state initialization. Neither
implementation receives container names, credentials, persistence paths or grants.

Host adapters own desired state, manual/uncertain pauses, crash limits, observations,
container generation fencing and all Docker effects. Idle healthy plans have no
timer. Component withdrawal retains pending state and pauses its effects; restored
availability rechecks current observations and resumes only eligible desired work.
No linked fallback occurs in live mode. Compatible updates use registered engine
publication after actual host adoption; shared contracts/host changes are coordinated.

The fixed webagent-boot-host launcher is a host authority executable, not a
replaceable implementation. It reuses src/engine_sandbox.rs to install Landlock,
seccomp, descriptor and memory limits before implementation exec. The daemon
drops UID/groups, clears the environment and verifies the launcher's protected
receipt. A whole process-group drain retires both launcher and service. The
launcher is installed only from the attested artifact of a successful coordinated
release; compatible policy publication leaves it unchanged.

The protected boot_host.py daemon subscribes to Docker container/health transitions
and inotify publication/control/job events. It performs no host/public network probes.
Only the fixed development API/loop/gate targets are controlled; existing production
supervision remains outside this conversion. Desired/manual/uncertain state is saved
before effects. A changed container identity/start generation aborts the effect.
Three recoveries per hour trigger a manual guard; sustained healthy recovery clears
attempts after one 120-second deadline. Docker transport faults use a 30-second retry.
Broker jobs conservatively fence autonomous effects until terminal. A protected
effects.lock lease also serializes host recovery/manual actions with broker catalog
switches and container deployment/rollback, closing the admission race. Host adapters
are installed as reviewed immutable broker modules; their permissions and root run
authority remain outside independently replaceable implementation packages.

The console is a status/control client. Network presentation is collected only while
visible, and black-screen status refresh sleeps until keyboard input. Host startup,
Docker-native exit recovery and bounded health recovery continue independently.
Foundation release 097d0a9c-f256-4eea-8c59-0f20abc6e98c passed 882 Rust
cases (five existing ignored), optimized linked/live equivalence and real host
startup/equivalence. Console activation preserves v3 container identities. The
Textual 8.2.8 input driver now blocks except for real parser deadlines and an
explicit shutdown wake; black presentation pauses timers across all screens.

Final adoption exposed a literal newline escape in the broker socket adapter.
The correction includes strict host-source hash/required-service adoption, the
boot shared-contract classification fence, and bounded host-source publication
for exactly installed boot authority bytes and their own docs/tests. Healthy
hosts ignore broker log writes; release-blocked faults remain retained and a
contended effect lease schedules only a real-fault retry. Runtime replacement,
withdrawal/selective restoration, final CPU and publication evidence remain pending.

## Live lifecycle verification
Compatible 0.1.1 publication ecdabf32-9cca-4a7d-9004-b82b98c64a8b replaced the
planner independently, reused unchanged context/background artifacts, preserved
API/loop/gate and daemon identity, retired the old generation, and matched three
stable-library cases. Withdrawal 415d30c1-0bad-47bc-a980-cabf96f57196 paused the real
host and preserved its pending state. Three stopped disposable containers had
zero effects, with manual-off and uncertain states retained. Restoration uses
the exact verified sealed 0.1.1 artifact; selective-start/cleanup evidence follows
the supported restoration receipt, rather than an inferred rollback or fallback.


Completion: restoration 9ea3dec4-8b12-41ca-bdf0-470c22689ad7 reused the exact
verified 0.1.1 artifact; one eligible real fixture started exactly once while
manual-off and uncertainty remained stopped, then all fixtures were removed.
A real unhealthy fixture verified grace deferral and one generation-fenced
restart with injected expiry; it was removed. Final three 20-second samples
recorded zero CPU ticks across daemon, launcher, planner and console. The real
tty was entirely blank with black backgrounds. Ordinary user wake and recovery
permissions remain in the host; no process/resource claim implies whole-server
CPU reduction. Full receipts and sampling limits are in engine-modularization.md.
