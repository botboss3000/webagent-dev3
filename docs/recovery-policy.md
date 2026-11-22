# Recovery policy

What WebAgent v3 does when a model call fails, when a bridge model's client is offline, when a
tool call is cut off, and when the app's processes restart. Each section says what happens, what
the user sees, and which setting controls it. Settings marked "live" are read from their file on
every use, so editing them needs no rebuild.

## 1. When a model call fails

Every failed model call is sorted into one failure class, and the class decides what happens next.
The rules live in `crates/modules/engine/src/runtime/failure.rs` and
`crates/modules/models/src/selection.rs`; the numbers come from the backup policy
(`config/chat/model-backup.json`, live, with a per-agent overlay edited through the configuration
composer or All Settings).

| Failure class | Examples | What happens |
| --- | --- | --- |
| Temporary | Connection error, HTTP 408/429/5xx, stream stalled, bridge client stopped responding | Retried on the same model, then switched to the backup model, then resumed later automatically |
| Quota used up | Provider usage limit reached | Switched to the backup model at once |
| Key rejected | Provider refused the credentials | Switched to the backup model at once |
| Model unavailable | Provider says the model does not exist; bridge with no client running | Switched to the backup model at once |
| Not retried | Malformed request, cancelled by the user, refused by policy, protocol error | The run fails |

The order of steps for one call:

1. **Retry on the same model** (temporary failures only): `same_model_retries` = 2 times, waiting
   `retry_delays_seconds` = 2 s then 8 s, plus up to `jitter_percent` = 25 % at random. A
   provider's own Retry-After wins when it is longer, up to `max_retry_after_seconds` = 60.
2. **Switch to the backup model.** Each model on an agent's Models page has an ordered list of
   backup models ("Aliases"). The call moves down that list to the next model able to do the call
   (for example one that can read images when the call has images). `switch_on` turns switching on
   or off per failure class; all four are on. The switch is silent: no error appears in the chat,
   only an activity row "Switched from A to B (reason)".
3. **Resume automatically.** When every model on the list has failed on a call that is safe to
   resume (a temporary failure, or an offline bridge, with nothing sent back yet and no tool effect
   in doubt), the run waits `auto_resume_after_seconds` = 180 and starts again with a fresh retry
   budget, `auto_resume_attempts` = 1 time. The activity row reads "Retries used up · resuming
   automatically in 3 min". Stop cancels the wait.
4. **Stop with a Resume button.** After that the run is stopped (not failed) and the chat offers
   Resume. Resume is refused when part of a reply had already arrived or a tool effect is
   uncertain (section 4).

A call is never retried or switched once part of its reply has arrived, or while an earlier tool
call's outcome is uncertain: the run fails instead, so nothing is done twice.

Other limits on model calls:

| Limit | Default | Where | Effect |
| --- | --- | --- | --- |
| `response_start_seconds` | 60 | `config/chat/model-backup.json` | A provider that has not started answering by then counts as a temporary failure |
| `streaming.stall_seconds` | 120 | `config/models/models-runtime.json` | A stream with no new data for this long counts as a temporary failure |
| `workflow.max_attempts` | 3 | `config/engine/loop-runtime.json` | Model calls allowed within one turn; reaching it pauses the run: after failed calls the chat shows "The model is unreachable" and administrators get a `model_unreachable` activity row with the model, provider, recovery tried and last error |
| `workflow.max_turns` | 80 | `config/engine/loop-runtime.json` | Turns allowed in one run (0 = no limit); reaching it pauses the run with "This run reached its limit of N steps" |

### Staying on the backup model, and switching back

Applies to every provider, not only bridges. Once a backup model has answered for a chat, the chat
stays on it for its later messages too, so that model's provider cache keeps working. The session
record `model_backup:{session}` remembers which model was replaced and by which backup; a new run
starts on that backup and shows the activity row "Switched from A to B (backup still in use)".

The chat tries its first model again at the next **major context reorder**: when the chat has been
idle longer than the model's cache timeout, or its tool definitions changed. At that moment no
cache is left to lose. The activity row reads "Switched from B to A (trying the first model
again)". If the first model still fails, the normal steps above move the chat back to the backup.
This switch-back is tried at most once per run.

Two consequences:

- With keep-alive switched on, the cache never times out, so the chat stays on the backup until its
  tools change or keep-alive ends.
- The System model (used for summaries and session names) keeps its own separate backup state and
  does not stay on a backup between runs.

## 2. Bridge models (Claude, OpenCode, Muse, Codex, Antigravity and Gemini Bridges)

A bridge model's calls are answered by `harness-bridge.py`, a script running on a paired computer
that uses the local `claude` or `opencode` command-line tool (no desktop app needed). WebAgent
queues each call; the script fetches it, confirms it, sends a progress signal while it runs, and
posts the reply. Code: `crates/modules/models/host/harness_bridge.rs`.

**Which computer answers.** Any number of paired computers ("clients") may serve one bridge
provider. A model answers on every client by default; with more than one client paired, the Models
page shows a checkbox per client, and a model with some unticked is offered only to the ticked ones.
"Offline" is then judged by those clients alone. The same model can be added once per client, with
one entry listed as the other's alias, so one computer backs up another.

**Online and offline.** The script keeps a request open to WebAgent at all times (each held up to
`poll_hold_seconds` = 10). A bridge with no request seen for `client_online_seconds` = 120 shows as
offline on the Models page.

| Situation | Noticed after | Result |
| --- | --- | --- |
| No script has checked in for 120 s | At once | Model unavailable, bridge offline: switch to the backup model |
| Call not picked up | `unclaimed_call_seconds` = 15 | If any script checked in since the call was queued: temporary failure, retried. Otherwise: bridge offline, switch at once |
| Call fetched but never confirmed | `offer_lease_seconds` = 15 (plus the 15 above) | Same rule as above |
| Script stopped sending progress signals | `heartbeat_timeout_seconds` = 15 (script sends one every `client.heartbeat_seconds` = 5) | Same rule: no check-in since the last signal means the script is gone, so switch at once |
| Script running but no reply at all | `answer_seconds` = 1800 | Temporary failure "Bridge reply timed out" |

All bridge timers are in `config/models/models-runtime.json` (`harness_bridge`, live). The script
receives the `client` values with every request, so changing them needs no reinstall.

**Offline bridge with no backup model.** The call is safe to resume (nothing was sent), so the run
waits 3 minutes, tries once more, and then stops with Resume. The Models page warns under the
backup list of any bridge model that has none.

**Nothing waits in the queue for a returning bridge.** A call that failed as offline is not picked
up later; the run moves on (backup, automatic resume or Resume). A restarted script needs no
re-pairing: its saved secret stays valid until revoked on the Models page.

**Summaries on bridge models** are made by forking the bridge's own session. A bridge that cannot
fork (no script online that supports it) sends the summary to the System model instead.

## 3. Planned, not built: provider status ping

Today WebAgent can tell only whether a bridge script is running. It cannot tell whether the
provider behind it (or any API provider) is answering, except by sending a real call. The bridge
script does ask its own throwaway question ("Reply with the single word OK") when a real OpenCode
call goes silent, but WebAgent never sees that check, and keep-alive pings run inside real chats.

Planned: a status ping per provider connection and bridge model, sent in a throwaway session (never
a user's chat), with the last result shown on the Models page. It would let the switch-back in
section 1 test the first model before moving a chat back to it, instead of trying a real call.
Not implemented; no setting exists for it yet.

## 4. Tool calls cut off or in doubt

| Situation | What happens | What the user sees |
| --- | --- | --- |
| A read was running when the app restarted | Run again from the start | Nothing |
| A write was running when the app restarted | Not run again. The action ledger marks it interrupted and the agent receives an "outcome unknown" result naming the cause (planned restart, restart for a deploy, unexpected restart), telling it to check for a partial change instead of resending | The run continues; the uncertain action shows in the Actions panel, where the user can mark it done or failed |
| The user pressed Stop, or the global shutoff came on, during a write | The run goes to `recovery` and holds the chat's queued follow-up messages | Recovery panel: Run again, Use my result, End run |
| An identical write is repeated later in the same run | Held as "Possible duplicate, not run" unless the agent says it is a deliberate repeat; high-risk tools then also need the user's approval | An approval request for high-risk tools |
| A background (detached) tool call lost its owner in a restart | Never re-run: a read is reported as failed, a write as outcome unknown | The agent's next turn |

Risk levels (exempt, none, low, high; high by default) and duplicate-protection windows come from
each tool's `tool.json`. Code: `crates/modules/engine/src/tools/retry_guard.rs`,
`crates/modules/engine/src/runtime/batch.rs`, `crates/modules/actions/`.

## 5. When the app restarts

The app runs as an API process and a separate agent-loop process (`webagent-dev-v3` and
`webagent-dev-v3-loop`), sharing the same databases.

**Who owns a run.** The loop claims a queued run with a 30-second lease, renewed every 5 seconds. A
run saved by a process whose lease has expired is refused, so two processes never advance the same
run.

**Planned restart (deploy or update).** The loop stops taking new work, records that the restart
was planned, and waits up to `scheduler.drain_timeout_seconds` = 180 for running runs to pause
between steps ("Paused for app update; resumes automatically"). Runs still going after that are
cancelled and recovered at startup as below.

**Startup.** Before the loop reports ready it restores the global shutoff state (a missing record
counts as shutoff on), then recovers every unfinished run:

| Run was | After restart |
| --- | --- |
| Queued | Stays queued and runs when its retry wait has passed |
| Running, between steps or mid model call | Queued again; the interrupted model call is simply sent again |
| Running a read | The read is reset and the run queued |
| Running a write | Settled as outcome unknown (section 4) and queued |
| Waiting for a build (`waiting_external`) | Follows its build job again |
| Paused because the restart interrupted context preparation (within 24 h) | Queued again, "Recovered after interruption" |
| Waiting for approval | Stays waiting. An approval past `tools.approval_expiry_seconds` = 86400 counts as denied; a changed authorization stops the run; in Auto mode a plain approval is queued |
| Stopped, paused, failed, completed, cancelled | Unchanged |

Any run that would be queued is stopped instead when automatic startup resume is off (the
administrator setting `boot_orphan_resume`, on by default, stored in the database: "Automatic
startup resume is disabled; explicit resume required"), the shutoff is on, the model is no longer
authorized, or the chat's participation changed.

While running, the loop repeats this check every `scheduler.recovery_interval_ms` = 10000 for runs
marked running that no process is executing. A run whose processing errors three times on the same
turn pauses with "Paused after 3 failed attempts".

**Unexpected hangs.** The loop writes a heartbeat every 2 seconds. Its watchdog warns after 120
seconds without progress and exits after 180 seconds so that Docker restarts it. `/health/ready`
fails when the loop heartbeat is older than 10 seconds. Host-level recovery of unhealthy
containers is described in `docs/boot-supervisor.md`.

**Background work after a restart.**

| Work | After restart |
| --- | --- |
| Context summary in progress | Marked failed ("Background summary interrupted by a restart"); the next trigger plans a new one. A summary that fails on the chat's model is retried on the System model |
| Keep-alive pings | Their records survive; pings resume within 30 seconds and are sent about 2 minutes before a cache would expire, never after it has expired |
| Session naming | Retried with its own limits (`config/configuration/prompts/session_naming.json`: `attempts_per_job` = 3, `recovery_retry_delay_seconds` = 60, `max_job_attempts` = 3); after that the "New:" title stays |
| Queued follow-up messages | Re-checked on every pass. After a failed or cancelled run the queue holds ("Previous run stopped or failed; resume the queue when ready") |
| Bridge calls waiting in the queue | Dropped; the waiting run fails over as in sections 1 and 2 |
| Broker builds and claim waits | Followed again |

**Settings files after a restart.** A live settings file that becomes invalid keeps serving its
last good version, but only in memory: after a restart there is no last good copy, so the invalid
file is rejected. For `config/engine/loop-runtime.json` this means the loop logs "Live runtime
configuration rejected" and runs nothing until the file is fixed.

**Power loss.** The databases use SQLite write-ahead logging with full sync, and the shutoff marker
and key files are synced to disk. There is no special handling for a settings file left empty by
a power loss; it is treated as invalid as above.

## 6. The chat page

The chat page follows a numbered event stream. When the connection drops it shows "connecting",
fetches a snapshot to fill any gap, and reconnects with waits growing up to 30 seconds; the server
ends each stream after an hour and the page reconnects. Retry and switch activity rows appear as
described in section 1, grouped under "retry" or "recovering". A run whose tool outcome is in doubt
shows the recovery panel (section 4) with its diagnostics collapsed. Code: `ui/chat/core/client.js`,
`ui/chat/core/activity.js`, `ui/chat/core/recovery-details.js`.
