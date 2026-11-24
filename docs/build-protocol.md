# Build protocol: what any new work must agree to (2026-11-23)

This is the one page a new build (by a session, an in-app
agent or the user) is checked against so it does not undo the unified performance, security and
dependency plan. It adds to the checkout-root `AGENTS.md` and the `codebase_admin` skill; it
replaces nothing in them. Sections A and B state the access and background-work rules in full.

## A. Access and identity (stream B)

1. **Access is decided from data that is fresh at decision time.** No time-based cache of any access
   decision, anywhere.
2. **Use the request `Identity`** (built once by the middleware, with its credential kind). Do not
   re-read the account, role or tier in a handler, and do not build identity from loose JSON fields
   in new code.
3. **Every record an access decision depends on has a plain integer revision** (`auth_epoch`,
   `agent_epoch`, session header, model catalog revision, ability catalog revision; the access
   policy uses its digest). A change to such a record goes through that record family's **one
   write function**, which raises the revision in the same transaction. Never write those tables
   or keys from anywhere else; the code-search guard tests fail the build if you do.
4. **Fail closed.** A missing or unreadable revision, identity or policy value means "changed" or
   "refused", never "allowed".
5. **Credential kinds keep their limits.** Widget and agent-browser sign-ins never carry
   administrator power; agent browsers are read-only; a disabled or unapproved account is refused
   on every route. A new route states which kinds may call it.
6. **On the model-call path, read once.** Take what you need from the `CallContext` built at the
   start of the model call; do not add another `live_access`, mode, ability-state or catalog read
   per call. The per-tool decision at execution time stays.
7. **Until the switch (B5): the old full check decides.** New code must not make the shadow
   comparison disagree; a new writer that forgets a revision shows up there.

## B. Background work and storage (stream C)

1. **No new timer that visits every account.** Background work is driven by a wake hint naming
   the account, or by a row in `account_due(user_id, kind, next_due_at)`; a slow spread-out sweep
   is the only fallback.
2. **Stored records stay the authority.** Hints and due times only decide *when to look*.
   A lost hint may cost delay, never a lost action.
3. **Write order across the two databases: the due row first, then the account's own record.**
4. **Plain columns hold no content.** Only ids, statuses, counts, flags, timestamps and revisions.
   Titles, messages, tool arguments and results stay encrypted.
5. **Do not decrypt a record to read a status, a count or an owner.** Use (or add) a plain index
   field written in the same transaction as the record.
6. **A hint carries no authority**: validate its format; it only causes a visit.
7. **Things that only grow need a retention rule** in the same change (ledgers, finished task
   records, codes, logs).

## C. Runs and limits

1. Run limits are checked at admission or claim, in the writer transaction, never in a scan.
2. A stopped, paused or parked run gives its run slot back and takes one again when it resumes
   (user decision 11-23); approval waits keep theirs.
3. Do not start a helper program by copying the agent loop process (no pre-exec hook in the loop);
   use the `--sandbox-exec` launcher.
4. Do not hold the loop's process-wide lock across a model call, a network call or a scan.

## D. Security boundaries that are not negotiable

1. No change weakens an access check, an approval, isolation or credential handling to gain speed
   or a smaller build. Every refusal stays a refusal; when a design reads otherwise, keep the
   decision and record the question.
2. Administrator-only pages live under `ui/main-panel/Administrator/` and are served only to a
   signed-in administrator. The broker's page check expects them to answer 404 without sign-in.
3. Root actions go through the host helper and the Host Admin approval. Nobody approves their own
   request. If a permission check refuses an action, the action stops; another session does not do
   it instead. Ask the user.
4. Credentials are generated and stored by the host or the user, never typed, printed, logged or
   passed in arguments. No real-looking credential literals in tests.
5. Settings that can be tuned live in the JSON config files, not in code.

## E. Where code goes and what a change must carry

1. New and changed code goes in its owning module crate (`src/` for rules and tests, `host/` for
   adapters), not in root `src/`. Performance and security come before build speed.
2. New dependencies: none without a reason written in the task; prefer the standard library or a
   crate already in `Cargo.lock`; no new outside service without a written note on what happens when it is unavailable.
3. The Core build must still compile (the broker checks it).
4. UI changes work at phone width and desktop.
5. A tool change updates that tool's skill in the same patch.

## F. Proof a change must bring

1. Tests for both sides of every rule it touches: the refused case and the allowed case; for
   access data, a change made from a second account or sign-in.
2. Numbers labelled **measured** or **read**. For anything on the request, model-call or idle
   path: statements and decrypts per model call, and idle processor share and statements per
   second, before and after, measured the same way both times; the handoff names the method.
3. Never more than 10 concurrent test runs on the development server; 20 only on the isolated
   tester copy of the app on the sidekick (never reset or remove it).
4. A handoff file listing what shipped (commits), the measurements, **every assumption made**, and
   what is left. A session's last reply starts with its `FINAL:` line.
5. The usual release rules of this checkout's `AGENTS.md` and the `codebase_admin` skill: broker task, healthy release, exact commit, claims
   released, no push without the user.
6. A queued apply is kept and applied at your turn when it still fits current source (receipt
   `rebased: true`); do not abandon and begin a new task because a release moved your files.

## Current targets

Under 80 statements and 30 decrypts per model call; over 100 test runs a minute; idle loop and API
each under 1 % of a core and under 50 statements a second.

Index: docs/index.md — update this doc's row there whenever you change it.
