# Capacity: how the server's processors, memory and run limits are shared

This page explains what limits how much work one WebAgent server can do, every setting that
controls it, how to choose values for a different server, and how to measure the maximum number
of active users. It is operator documentation: it says nothing about accounts' commercial terms.

Labels: **read** means taken from the code or configuration; **measured** means taken on the
development server on the date given. Anything else is guidance, not a measurement.

## 1. The design in one page

The app runs as a few containers on one machine. Each has a job and its own limits:

| Process | What it does | What limits it |
| --- | --- | --- |
| **API** (`webagent-dev-v3`) | Every page, request and message admission. The tool worker (the isolated Rust process that runs approved low-level tools: file tools, commands, terminals, previews, session Git) lives in this container too, unless the worker container below is switched on. Each request is small, but it is the part people feel. | Processor weight (highest), optional processor cap, memory, task limit. |
| **Worker** (`webagent-dev-v3-worker`, off by default) | The tool worker in its own container (`scheduler.worker_container`, section 2.1): users' commands and terminals then use its memory, not the API's. | Processor weight (below the loops), optional cap, memory, task limit. Commands run one at a time per loop process, so its memory need is about (loops + 1) × the per-command limit (measured 2026-11-24: one 200 MB command added 210 MiB; eight at once from one loop still peaked at one). |
| **Agent loops** (`webagent-dev-v3-loop`) | Runs the agents: context, model calls, tool calls, recovery. The container runs `scheduler.loops` loop processes under a small supervisor; accounts are split between them. | Each loop process can use about **one processor thread** (its run steps share one in-process lock), so N loops use up to about N threads. Also the run ceiling and per-agent caps below. |
| **Browser** (`webagent-dev-v3-browser`) | The private Chromium browsers agents use. | Processor weight, optional cap, memory, task limit, and the browser limits (how many run at once). |
| **Build jobs** (the broker's compiles and tests) | Building and testing new releases. | They run only on processor time the app leaves idle (`builds.cpu_weight: "idle"`, the default), plus the broker's own build budget (`cpu_budget_millis`, `cargo_build_jobs`, `test_threads`). |
| **Shared databases** | SQLite: one `app.db` for the installation and one database per account. | Writes to one database happen one at a time; the number of account databases held open per process is capped. |
| **Model providers** | The outside services that answer model calls. | Their own rate limits and latency, which no setting here changes. With real models this, not the processors, is usually the first limit. |

**Processor sharing is by weight, not by reservation.** A weight only matters when the processors
are all busy: then each container gets processor time in proportion to its weight. When the
machine is not full, any container may use any idle processor. So the API, which has the highest
weight, keeps answering quickly while the loops or a build are busy, and the loops can use every
idle thread when nothing else wants it. Hard processor caps are optional and off by default,
because a cap leaves processors idle while a capped container waits.

Weights compete between neighbours in the Linux control-group tree. The app containers sit side by
side in `system.slice` (with other system services at the normal weight 100); build jobs run in
`webagent.slice/webagent-build.slice/webagent-build-<instance>.slice`, and the build weight is set
on all three of those, so builds compete with `system.slice` as a whole at a low weight.

Measured on the tester copy of the app, 2026-11-24 (one loop process; each command touched 200 MB
and kept one thread busy for 15 s): commands from different chats run one at a time per loop
process (8 at once took 8 × 16 s); with the worker in the API container the API's memory rose by
about 210 MiB per command and never by more than one command; with the worker container switched
on, the same command raised the worker container from 4 to 211 MiB while the API stayed at
119–157 MiB.

Measured on the development server, 2026-11-24 (12 threads; one loop process; the probe was
`/health/ready`, which runs four database queries in the API, timed 40 times 0.2 s apart; the load
was 12 shell loops spinning inside the loop container for 15 s; other build work was running on the
server at the same time, so the numbers are noisy):

| | Before: hard caps (API 4, loop 4, browser 2 processors), no weights | After: weights 400 / 100 / 100, no caps |
| --- | --- | --- |
| Processors the loop used while spinning alone | 4.00 (held at its cap) | 11.17 |
| API idle: median / 90th percentile | 1.4 ms / 1.8 ms | 1.6 ms / 4.1 ms (later 2.8 / 5.3 and 3.1 / 8.9 as other work rose) |
| API while the loop uses all 12 threads | 2.8 ms / 18.5 ms | 12.7 ms / 17.6 ms |
| API while the loop uses 11 threads | — | 4.8 ms / 10.1 ms |
| API while the loop uses 10 threads | — | 3.7 ms / 6.9 ms |
| All 12 threads, API weight raised to 10000 | — | 12.0 ms / 17.9 ms |

What this shows:

- Without the cap, idle processors are used: the loop went from 4 to 11 processors.
- **A weight decides the share of processor time, not how fast a waking request gets a
  processor.** When every thread is busy, an API request waits for running work's time slice at
  each step, whatever its weight (10000 did no better than 400). Keeping one or two threads free
  keeps the API close to its idle times.
- Build jobs therefore do not rely on a weight: by default they run as "idle" work, which the
  kernel schedules only on processor time nothing else wants (`builds.cpu_weight`, section 2.1).
  On an otherwise idle machine a build still uses every thread it is given and takes its normal time.
- So size the busy work to leave room: the loop processes use about one thread each, so keep
  `scheduler.loops` at most C − 2; the build budget already leaves one thread (`cpu_budget_millis`
  defaults to C − 1 cores). If something in the loop container can use every thread (many agent
  commands at once, for example), set `loop.cpu_cap` to C − 2: a cap that only bites when the
  machine is nearly full.

## 2. Every capacity setting

"Restart" below means what must happen before a change takes effect.

### 2.1 Containers: processor weight, optional cap, memory, tasks

File: the installed broker configuration `/etc/webagent/<instance>/build-service.json`
(root-owned), section `scheduler.container_capacity`. Defaults are in
`build-service/scheduler-defaults.json`; a role or key you leave out keeps its default. The broker
refuses to start with a value outside its range (the broker's README, "App container capacity",
has the same table).

| Key | Default | Range | What it does |
| --- | --- | --- | --- |
| `api.cpu_weight` | 400 | 1–10000 | API's share when processors are contended. 100 is normal. |
| `loop.cpu_weight` | 100 | 1–10000 | Agent loops' share. |
| `browser.cpu_weight` | 100 | 1–10000 | Browser container's share. |
| `worker.cpu_weight` | 50 | 1–10000 | Worker container's share (when switched on): below the loops, so a user's command never slows chat, above builds. |
| `<role>.cpu_cap` | `null` (no cap) | `null` or 1 to the number of processor threads | A hard limit in processors (may be fractional, such as 1.5). The loop's cap, if set, is raised to loops + 1 so every loop process and its helpers fit. |
| `api.memory_bytes` | 2 GiB | 512 MiB to host memory | Memory limit. |
| `loop.memory_bytes` | 8 GiB | 1 GiB to host memory | Memory limit for all loop processes together. |
| `browser.memory_bytes` | 3 GiB | 1 GiB to host memory | Memory limit for all browsers together. |
| `worker.memory_bytes` | 2 GiB | 512 MiB to host memory | Memory limit for all commands, terminals and previews together (worker container only). |
| `<role>.pids_limit` | API and loop `null` (= `api_pids_limit`, 1024 on development); browser and worker 1024 | `null` or 256–65536 | Task (process and thread) limit. The loop's is raised to 512 per loop process. |
| `worker_container` (in `scheduler`, not in `container_capacity`) | `false` | `true`/`false` | Runs the tool worker in its own container `<api>-worker` from the next release. It takes effect only once the host has the worker network `webagent-worker` and its firewall script (broker README, "Worker container"); until then releases keep the worker inside the API container. |
| `builds.cpu_weight` | `"idle"` | `"idle"` or 1–10000 | Priority of all build jobs together. `"idle"` is systemd's `CPUWeight=idle` (the control group's `cpu.idle` flag): build jobs get a processor only when nothing else wants it, so an app request never waits behind a compile. A number makes it an ordinary weight instead (20 was the earlier default). |

A default memory value larger than a small machine's memory shrinks to that machine's memory; a
value you set yourself is checked instead. Each container keeps the swap allowance it already had.
Nothing here changes the containers' isolation: user, mounts, network, read-only file system and
dropped privileges stay as they are.

How weights become Docker values: Docker takes "CPU shares", and its runtime (runc 1.3 or later)
converts shares to the control-group weight (1024 shares = weight 100). The broker converts your
weight to the shares that produce it. Check the result in
`/sys/fs/cgroup/system.slice/docker-<container id>.scope/cpu.weight`.

**How to change it, and what restarts:**

1. Change the installed file through Host Admin (`buildctl host request file_write` with the
   whole file, only that section changed; never a hand edit with sudo). `host_revert` of that
   request is its undo.
2. Restart the broker while it is idle (Host Admin `unit_restart` of
   `webagent-build-<instance>.service`; `buildctl status` shows `busy`).
3. At start the broker updates the running app containers live with `docker update`: no container
   restart, no rebuild, nobody is signed out. Its log prints `Container capacity <role>: updated`.
   Every later start of an app container by the broker (release, browser sync, rollback, restore)
   applies the same values, and releases create new containers with them.
4. One exception: a processor cap added back after a cap was removed live waits for the next
   release (Docker keeps a live-removed cap removed across restarts; the log says so).
5. The build priority (`builds.cpu_weight`) is set when the broker next admits a build. Check it
   in `/sys/fs/cgroup/webagent.slice/cpu.idle` (1 for `"idle"`) or `cpu.weight` (a number).
6. **A broker install writes the defaults it knows into the installed file.** So a default that a
   later release changes does not apply on a server that was installed before it: the installed
   file then holds the old value explicitly. Change the value in the installed file instead. On
   the development server `builds.cpu_weight` was such a case: the installed file said `20` until
   2026-11-24, when it was set to `"idle"` (Host Admin request `41ee9b0b`; measured afterwards:
   `cpu.idle` 1, the next build in its normal time, API `/health/ready` 1.3–1.8 ms median during
   it against 1.6 ms idle).

### 2.2 Agent loops and runs

| Setting | File and key | Default | What it does | Restart |
| --- | --- | --- | --- | --- |
| Number of loops | `config/engine/loop-runtime.json`, `scheduler.loops` (absent = 1) | 1 | Loop processes in the loop container; each uses up to about one processor thread. 1–16. | Read when the loop container starts: `docker restart webagent-dev-v3-loop` or the next release. For more than 3 loops run a release, which also raises the loop container's processor and task limits. |
| Run ceiling | same file, `scheduler.max_active_runs` | 10 | Agent runs executing at once on the whole server, across all loops. 1–64. | Read when the loop process starts. |
| Background workers | same file, `scheduler.max_background_workers` | 4 | Background tasks (detached tool calls, follow-ups) at once, per loop process. | Loop start. |
| Media requests | same file, `scheduler.max_media_requests` | 8 | Media (voice, images) requests at once. | Loop start. |
| Administrator reserve | `config/access/policy.json`, `run_limits.reserved_admin_runs` | 1 | Run slots ordinary accounts cannot take, so an administrator can always start a run. | Read live. |
| Per-agent cap | same file, `run_limits.max_active_runs_per_agent` | `null` = half the run ceiling, at least 2 | Active runs one agent may have at once for non-administrators. | Read live. |
| Command budgets | same file, `run_limits.concurrent_commands_per_run` (2), `concurrent_commands_per_user` (4), `command_cpu_seconds_per_run` (3600), `command_cpu_seconds_per_user_day` (14400) | as shown | How many shell commands run at once and how much processor time they may use. | Read live. |
| Open account databases | `config/storage/storage-settings.json`, `user_database_pools.max_connected` | 192 | Account databases holding open connections at once in each process (each connection is a thread). It is a hard bound: a request that finds every slot in use waits for an idle database to give way. At least 8. | Read at process start: the next release. |
| Connections per account | same file, `user_database_pools.max_connections` | 2 | Connections one account database may hold. Account threads ≈ `max_connected` × this. 1–8. | Process start. |
| Idle account lifetime | same file, `user_database_pools.idle_seconds` | 5 | Seconds before an idle account connection closes. 1–60. | Process start. |
| Connect wait | same file, `user_database_pools.connect_wait_ms` | 5000 | How long a request waits for a free account-database slot before it is refused with a retryable 503 (`Retry-After: 2`). 0–10000. | Read live. |
| Runtime blocking threads | same file, `process.max_blocking_threads` | 64 | Threads the runtime may add for blocking work; each is one task against the limit. 8–256. | Process start. |
| Browser limits | `config/browser/browser-limits.json`: `max_running_per_app` (5), `max_running_per_user` (4), `idle_seconds` (180), `queued_idle_seconds` (30), `max_wait_seconds` (600); also `min_extension_version` (0.2.0), the oldest Browser Extension that may take actions | as shown | Private browsers running at once, per server and per user, and how fast idle ones close while others wait. | Read live for each new browser. |

The JSON files under `config/` are the app's single live settings files. Change them in the
administrator's All Settings pages or through a broker task; each has a settings descriptor that
states its range.

## 3. Choosing values for a server with C processor threads and M GB of memory

1. **Loops:** one loop per processor thread you want agents to use, leaving room for the API, the
   browser and the system. A starting point is `loops = max(1, C/2 - 1)` on a machine that also
   builds, `C - 2` on one that does not. Measure before raising it (section 4).
2. **Run ceiling:** with real models a run spends most of its time waiting for the model, so the
   ceiling is usually set by memory, the model providers' rate limits and the per-agent cap, not by
   processors. Raise it in steps and measure.
3. **Weights:** keep API highest (400), loop and browser normal (100), builds `"idle"`. Raise the
   browser's weight only if browser tools feel slow while agents are busy.
4. **Caps:** leave them off unless the machine is shared with something that must be protected,
   or the loop container can fill every thread: then cap the loop at C − 2 so the API always finds
   a free thread (section 1 shows why a weight alone does not do that).
5. **Memory:** keep about 15 % of M (at least 1 GB) for the system. Of the rest, the loop needs the
   most (context and tool results for every run in progress): give it about half, the browser
   about a quarter (each Chromium needs a few hundred MB), and the API 1–2 GB.
6. **Tasks:** 1,024 per container is enough, and the broker raises the loop's to 512 per loop
   process. Budget the API's threads rather than raising it: account databases take at most
   `max_connected` × `max_connections` (192 × 2 = 384 in the shipped file), plus the application and agent
   pools (about 10), the runtime's workers (C) and its blocking pool (`process.max_blocking_threads`,
   64). That is about 470 threads on a 6-core server; a limit of 1,024 leaves room for tool
   processes, previews and browsers started by tools. Raise `api.pids_limit` only together with
   that budget: a server with 64 GB or many browsers may need 2,048, while one with 8 GB should
   lower `max_connected` and keep 1,024. Measured on the sidekick tester, 2026-11-24, four loops,
   400 users, a 20 s model: before the bound held, the API reached 1,011 threads, 362 account
   databases and refused 44 requests with HTTP 500; after it, see the verification in
   `work/unified-plan/api-task-limit.md`.
7. **Builds:** if the server builds, keep `cargo_build_jobs` at most C − 2 and `test_threads` at
   most C − 4 (broker README, "Tuning settings"); the low build weight keeps the app responsive
   even then.

### Worked example: the development server (6 cores, 12 threads, 31 GB)

| Setting | Value | Why |
| --- | --- | --- |
| Weights API / loop / browser / builds | 400 / 100 / 100 / idle | Defaults. |
| Caps | none | Use idle threads. |
| Memory API / loop / browser | 2 / 8 / 3 GiB | Defaults; about 18 GB stays for builds (the compiler takes up to 12 GiB) and the system. |
| Loops | 2 since 2026-11-24 | User decision; the broker also builds here (`cargo_build_jobs` 10). The loop container's 1,024 tasks fit two loops (start-up peak 285 tasks). |
| Worker container | on since 2026-11-24: 2 GiB, weight 50, 1024 tasks | The host has the worker network `webagent-worker` and its firewall unit (installed by the user); users' commands, terminals and previews no longer use the API's memory. |
| Browsers at once | 5 per app, 4 per user | User decision 2026-11-24. |
| Run ceiling, reserve | 10, 1 | Defaults; raise with more loops after measuring. |
| Open databases | 64 | Default. |

In force on development since 2026-11-24: builds `"idle"`, browsers 5 per app, **two agent loops**
(`scheduler.loops: 2`, switched on 09:25 by a live configuration save and a loop-container restart),
run ceiling 10 with 1 place kept for administrators, worker container on (`scheduler.worker_container:
true`, release `82a6f9a0`). Checked at two loops: the `multi_loop` test group passed; both loops serve
(`loop_members` rows 0 and 1, readiness 200); runs of the administrator (loop 0) and of ordinary
accounts on loop 0 and loop 1 start as fast as at one loop (claimed in 0.4–1.2 s); five ordinary
accounts at once ran 2 on loop 0 and 3 on loop 1 and all completed; a loop-1 run cut off by a
loop-container restart recovered with exactly one reply; a 60 s reminder fired exactly once for an
account on each loop; a loop-1 account's sandbox command ran in the worker container; with six long
runs on one agent five ran across both loops and the sixth waited while an administrator's run
started; with 12 runs asked for, exactly 10 ran at once. Idle cost: the two loops together used
about the same processor time as one loop (4.1 % against 3.9 % of a core over 10 minutes while
other work ran). Session naming works on both loops (title 2 s after the reply) once the default
agent's "system" model role is an API model; with the bridge model it had no answer at either loop
count. The Dashboard and Builds pages show no loop count or
per-loop state (the Dashboard's run counts cover only the signed-in account's runs on the current
agent). To go back to one loop: remove `loops` from `scheduler` (administrator configuration
save of `loop_runtime`) and restart the loop container; remove it as well before rolling back to
a release older than `a79e8f1`.
Checked with the worker container on: from its network the host, Tailscale peers,
100.100.100.100, the LAN gateway and the app containers are unreachable and the internet works;
commands of the administrator and of an ordinary account run as the worker user (1103, group
1105) with `/state` refused; file tools, a terminal, session Git, a private browser page and a
preview tunnel work; a run waiting for approval survives restarts of the worker container and the
API; one 200 MB command raised the worker container from 6 to 212 MiB while the API stayed at
about 235 MiB. To switch off: set `worker_container` back to `false` in the installed broker file
(Host Admin `host_revert` of that write), restart the broker while idle, then any `build_deploy`.
Ability-only and Markdown releases (`plugin_deploy`) need broker `0ad17c1` or later, which probes the
worker on the API's own socket.

### Worked example: a small cloud server (2 processor threads, 4 GB, no builds)

| Setting | Value | Why |
| --- | --- | --- |
| Weights | 400 / 100 / 100 (builds unused) | Same priorities. |
| Caps | none | Two threads are too few to split. |
| Memory API / loop / browser | 1 GiB / 1.5 GiB / 1 GiB | Leaves about 0.5 GB for the system. |
| Loops | 1 | One thread for the loop, one for everything else. |
| Run ceiling | 4–6 | Memory-bound; measure. |
| Browser limits | `max_running_per_app` 2, `max_running_per_user` 1 | Each browser needs a few hundred MB. |
| Open databases | 16–32 | Fewer threads per process. |
| Task limits | 512 | Enough for one loop at this size. |

## 4. Validating the maximum number of active users

This section gives the **method** only. The results table is filled in by a later validation.

**Definitions.** A *run in progress* is an agent run queued, running or waiting for a tool. An
*active user* is someone who sends messages and reads replies in a given period. The server's
limit is how many runs it can keep in progress at once without people's waits growing, so active
users are derived from runs in progress.

**Tools in the repository:**

- the `loadtest` model provider (`config/models/providers/loadtest.json`,
  `crates/modules/models/src/transport/loadtest.rs`): the server answers model calls itself. A
  message's lines program it: `TOOL <name> <json>` makes a tool call, `REPLY <text>` sets the
  answer, `THINK <ms>` waits before each turn (up to 60 s) to stand in for model latency. Nothing
  leaves the server;
- the authenticated app API (sessions and messages) to send load and read replies;
- the broker's `performance_work` tool for fixed benchmarks and processor profiles;
- the control-group files of each container (`cpu.stat`, `memory.current`, `pids.current`) for
  processor, memory and task use.

The load driver that sends many sessions' messages at once and records timings is not in this
repository yet; until it is, any script that uses the app API as above works.

**Steps (on a copy of the app, never on a server people are using):**

1. **Idle cost.** With nothing running, record each container's processor use over 10 minutes and
   the statements per second. Targets: each under 1 % of a core and under 50 statements a second.
2. **Throughput per loop (instant model).** With `loadtest-echo` and no `THINK`, send runs from
   many sessions as fast as they are accepted. Record completed runs a minute, the loop's
   processor use (expect about one thread per loop) and the API's request times. Repeat for
   loops = 1, 2, 4.
3. **Runs in progress at once (delayed model).** With `THINK` set to a realistic model time (for
   example 5–20 s) and a few tool calls per run, raise the number of concurrent runs in steps
   until runs start to queue (a run waits before its first model turn) or the API's request time
   leaves its idle range. That number is the most runs in progress at once.
4. **Check the other limits** while doing step 3: memory of each container, task counts, open
   databases, browser queue, and the API's 90th percentile request time.
5. **Record** for each step: date, release, machine (C, M), every setting in section 2, the number
   of loops, completed runs a minute, most runs in progress without queueing, the API's median and
   90th percentile, and the peak memory and tasks of each container.
6. **Derive active users.** Measure, on real use, the fraction of the time an active user has a run
   in progress (their *duty fraction*). Then active users ≈ most runs in progress ÷ duty fraction.
   Real model providers' rate limits apply on top and must be checked separately.

**Results (not yet measured):**

| Machine | Loops | Settings | Runs a minute (instant) | Most runs in progress | API p50 / p90 | Duty fraction | Active users |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Development, 12 threads, 31 GB | — | — | not yet measured | not yet measured | not yet measured | not yet measured | not yet measured |
| Small cloud, 2 threads, 4 GB | — | — | not yet measured | not yet measured | not yet measured | not yet measured | not yet measured |

## 5. More than one machine

Worker machines (a second machine taking browser work, commands, voice or local models, and
possibly loops) are planned and not built. Today everything above runs on one machine.

Index: docs/index.md — update this doc's row there whenever you change it.
