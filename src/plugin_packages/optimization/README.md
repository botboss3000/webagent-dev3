# Live Optimization plugin

/optimize <request> starts a measured resource experiment. It excludes prompts,
skills and tool-selection changes. Rust pins settings and prospectively checks
inactive Discoverable dependencies before session activation. Off, suppression
and denial remain authoritative. Deployment is denied during optimization;
request a separate release after reviewing the candidate.

Canonical source: src/plugin_packages/optimization. The tested image supplies
/usr/local/bin/webagent-optimization. Install this executable and plugin.json,
optimization.json, optimization.skill.md, measure.py and workload.py together
under plugins/optimization. Use one administrator owner, no group/world writes,
and an executable mode. Source alone does not install the executable.

The API scans every two seconds. Add starts, move/remove withdraws and stops,
atomic executable replacement updates. Invalid, partial and symlink packages
fail closed. Restart never reinstalls an absent package. API and loop share
fresh readiness and bounded UID-checked IPC. Binaries are administrator-trusted
application code. All six assets enter the generation hash; the supervisor
executes a private pinned copy. Ephemeral files live in data/optimization.
Durable broker receipts and artifacts remain under build-jobs/performance.

Agent preferences use encrypted revision-fenced ability settings and the
Optimization panel. Defaults: balanced priority, 3 candidates, 900 seconds,
both profiles, 1 CPU, 1024 MiB, standard coverage, zero slowdown/memory
regression and dependency changes disabled.
Leading --key=value flags override only this run: priority, attempts,
total_seconds, profiling, cpu_cores, ram_mb, coverage, max_slowdown_percent,
max_memory_regression_percent, allow_dependencies. Use -- before literal
flag text. Bounds: attempts 1–10, seconds 1–1800, CPU 1–2, RAM 128–4096 MiB,
regression 0–25 percent. Prompt overrides never persist.

performance_work requires an open isolated task, current input digest and
stable request ID. First benchmark uses role=baseline; candidates require a
successful baseline and use role=candidate. Profiles require a successful
benchmark of identical source. Status/cancel/artifact use the original request
identity. Artifact pages select recorded basenames with offset and limit.
The broker pins image, harness and effective settings, reserves cumulative
time before dispatch and enforces CPU/RAM/network/process limits. Interrupted
reservations stay charged. Shared compilation caches bind task/image/harness.
Candidates compare against original and best results within regression limits.

The driver runs the full locked native v3 correctness suite before measuring
selected real runtime tests for quick/standard/extended coverage. Plain wall,
child CPU and independent child peak RSS remain separate from instrumentation.
Callgrind gives instruction/call-path evidence, not sampled CPU time. Heaptrack
gives allocation counts and heap sites, distinct from RSS. Unsupported tooling
and resource exhaustion are explicit. Conservative defaults may not fit a cold
build; limits and correctness are never silently weakened.

Unit and real Docker revocation checks are in tests/test_optimization_measure.py
and tests/test_performance_work.py. tests/optimization_plugin_acceptance.py
runs real isolated API/loop roles and native capability probes. Actual v3
workload/profiler and authenticated model journeys remain release acceptance.
