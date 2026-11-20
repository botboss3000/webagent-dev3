#!/usr/bin/env python3
# ============================================================================================
# SETUP GUIDE — for the person or coding agent (for example Claude Code) installing this file
# ============================================================================================
# What it is: the client for a WebAgent "bridge" model (Claude Bridge, OpenCode Bridge, Muse
# Bridge). WebAgent queues each model call for this bridge; this script fetches it, runs it on
# this computer with the matching local CLI harness, and posts the answer back. It never runs
# WebAgent's tools itself: it only returns the model's reply and requested tool calls.
#
# Requirements:
#   - Python 3.9+ (standard library only; nothing to pip install).
#   - Claude Bridge: the `claude` CLI on PATH and signed in (`claude` once interactively, or
#     `claude login`). Calls run as `claude -p` with no CLAUDE.md, settings, hooks or skills.
#   - OpenCode Bridge: the `opencode` CLI installed and signed in.
#   - Muse Bridge: no CLI. A Muse agent on this computer must answer handoff files: for each
#     work/harness-bridge/sessions/<chat>/muse-inbox/<call>.request.json it writes the reply JSON
#     to the handoff's reply_file within 25 minutes (Muse's queue watcher wakes the agent).
#   - Outbound HTTPS to the WebAgent server (default https://dev.webagent.live).
#
# Install:
#   1. Make a folder for it, e.g.  mkdir -p ~/webagent-bridge/bin
#      and save this file there:   ~/webagent-bridge/bin/harness-bridge.py
#      The client keeps its state in ../work/harness-bridge/ next to that folder
#      (here ~/webagent-bridge/work/harness-bridge/): client secret, per-chat sessions, timing log.
#   2. Pair it: on WebAgent's Models page start adding a model, choose the bridge provider (for
#      example OpenCode Bridge), press "Pair a client" and run the command it shows within 10 minutes:
#        python3 ~/webagent-bridge/bin/harness-bridge.py pair https://dev.webagent.live ABCD-EFGH --name "My laptop"
#      The code works once. The client saves its own secret to work/harness-bridge/client.json
#      (mode 0600); treat that file like a password. Revoke a client from the same Models page.
#   3. Check the install:  python3 ~/webagent-bridge/bin/harness-bridge.py selftest
#   4. Run it and keep it running (it long-polls forever; WebAgent shows the bridge offline
#      within about a minute of it stopping):
#        python3 ~/webagent-bridge/bin/harness-bridge.py auto
#      For a permanent install use a user service, e.g. systemd (Linux):
#        ~/.config/systemd/user/harness-bridge.service
#          [Service]
#          WorkingDirectory=%h/webagent-bridge
#          ExecStart=/usr/bin/python3 -B %h/webagent-bridge/bin/harness-bridge.py auto
#          Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
#          Environment=PYTHONUNBUFFERED=1
#          Restart=always
#          RestartSec=10
#          [Install]
#          WantedBy=default.target
#        systemctl --user daemon-reload && systemctl --user enable --now harness-bridge.service
#      On macOS use a launchd agent or run it in a persistent terminal/tmux session.
#   5. Verify: send a message in WebAgent with the bridge model; the reply should arrive and
#      `python3 harness-bridge.py stats` should count the call.
#
# Isolation and guardrails (recommended; set these up with the user, then VERIFY them):
#   WebAgent never runs its tools here: this client only returns the model's reply and the tool
#   calls it asked for, and WebAgent runs those on its own server under its own permissions and
#   approvals. What runs here is the local harness (claude / opencode) on WebAgent's prompts, so
#   keep it away from this computer's other data:
#   a. One dedicated home per bridge. Run the client with HOME set to a folder of its own, for
#      example ~/webagent-bridge/home, so the harness keeps its sessions, database, settings and
#      login there instead of in your own ~/.claude or ~/.local/share/opencode (and never mixes
#      with your own interactive sessions):
#        mkdir -p ~/webagent-bridge/home
#   b. Sign the harness in inside that home (the user types their own login; never copy or print
#      credentials for them):
#        HOME=~/webagent-bridge/home claude            # Claude Bridge: sign in, then exit
#        HOME=~/webagent-bridge/home opencode auth login   # OpenCode Bridge
#   c. The harness's own tools stay off. This script already passes: Claude `--tools Read` with only
#      `mcp__harness` allowed (headless, so Read outside the run's folder is refused); OpenCode its
#      own tools as "deny" (paid models) or "ask" (free models, refused because nobody can answer).
#   d. A system-level sandbox for the service (Linux systemd; the system enforces it, whatever a
#      model asks). Add to the [Service] section of the unit above, adjusting the CLI paths:
#          Environment=HOME=%h/webagent-bridge/home
#          ProtectSystem=strict
#          ProtectHome=tmpfs
#          BindPaths=%h/webagent-bridge
#          BindReadOnlyPaths=-%h/.local/bin -%h/.opencode -%h/.local/share/claude
#          PrivateTmp=yes
#          NoNewPrivileges=yes
#      ProtectHome=tmpfs hides the real home; only the bridge folder (read-write) and the CLI
#      install folders (read-only) are visible. On macOS, run the client as a separate user account.
#
#   Agents setting this up: do not trust these comments; check each guardrail on this computer and
#   tell the user the result of each check:
#     - The service runs with the sandbox:  systemctl --user show harness-bridge.service -p Environment
#       -p ProtectSystem -p ProtectHome -p NoNewPrivileges   (expect HOME=…/webagent-bridge/home,
#       strict, tmpfs, yes).
#     - The real home is hidden from it: start a throwaway unit with the same settings and try to
#       read a private file, e.g.
#         systemd-run --user --pipe -p ProtectHome=tmpfs -p BindPaths=%h/webagent-bridge \
#           -p ProtectSystem=strict ls ~/.ssh      (expect "No such file or directory")
#     - The harness uses the dedicated home: after one call, its sessions appear under
#       ~/webagent-bridge/home (for example home/.claude/projects or home/.local/share/opencode),
#       not under the real home.
#     - The harness's own tools are refused: read this script's claude_command / opencode_config,
#       then ask the bridge model in WebAgent to run a shell command itself (not through a WebAgent
#       tool); the run must not execute it.
#     - `python3 harness-bridge.py selftest` passes, and one WebAgent message gets a reply.
#   If a check fails, stop and tell the user which guardrail is missing before serving calls.
#
# Options: HARNESS_BRIDGE_URL=<origin> overrides the paired server. Any number of clients, and
# processes of one client, may serve the same bridge provider: each call runs on exactly one of them.
# To serve two bridge providers (for example Claude Bridge and OpenCode Bridge), pair a copy in its
# own folder for each, so their work/harness-bridge/ state stays separate.
# Updating: download the file again from the Models page and restart the client.
# ============================================================================================
"""Harness bridge client: answer WebAgent v3 model calls for one paired bridge provider.

Common to every bridge provider (Claude Bridge, OpenCode Bridge, Muse Bridge).
Download it from a bridge provider on the Models page and run it on the
computer that answers the calls (Python 3 standard library only), then pair it.
Every call is acknowledged before it runs, so any number of clients may serve one
model. WebAgent sends each call's full chat-completions body
and the bridge provider it is for; this client saves it, works out what is new
since the previous call of the same WebAgent chat, and runs that provider's
harness (HARNESSES below).

    pair  URL CODE [--name NAME]  pair this client with a bridge provider (all of its models) using the Models page's code
    next  [--session ID]          wait for the next call; save it, print the delta
    reply CALL_ID FILE|-          post {"content": str, "tool_calls": [{"name", "arguments": {..}}]}
    auto  [--session ID]          answer every call with its provider's harness, resumed per WebAgent chat:
                                  claude_bridge: headless `claude -p` (needs `claude` signed in);
                                  bare: no CLAUDE.md, settings, hooks, memory, skills.
                                  opencode_bridge: `opencode run` (needs `opencode` signed in);
                                  its own tools stay listed but rejected for free models (they require them)
                                  and are hidden for paid ones.
                                  muse_bridge: hands each turn to a live Muse agent through files in the
                                  chat's muse-inbox/ (see answer_muse).
    stats [HOURS]                 summarize auto-mode call timing (work/harness-bridge/timing.jsonl), default 24 h

Session forks (auto mode, Claude and OpenCode only): when the CLI can fork a session (Claude Code's
--fork-session, OpenCode 2.0.32+'s `run --fork`) this client tells WebAgent so. WebAgent then asks for a
context summary as a fork call: answered from a fork of the chat's session, so it reuses its cached
prefix, without changing the session, state.json or the chat's tool files. After a fold the chat's
next session starts from a checkpoint session holding the summary blocks it shares with the previous
call, so a later fold forks that checkpoint instead of resending them. Muse never forks.

Files live in ../work/harness-bridge/ relative to this script's folder (below: work/harness-bridge/).
Client secret and server: work/harness-bridge/client.json (mode 0600), written by `pair`.
Server override: $HARNESS_BRIDGE_URL.
Per-chat files: work/harness-bridge/sessions/<webagent-session>/. `usage [--json]` reports what the
bridge keeps on this computer (per-chat folders and the harnesses' session stores); `cleanup [--dry-run]`
runs the clean-up that also runs on its schedule (cleanup.json; daily 03:00 by default).
"""
import base64, calendar, difflib, hashlib, json, os, re, secrets, shutil, signal, socket, subprocess, sys, tempfile, threading, time, urllib.error, urllib.parse, urllib.request, uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "work" / "harness-bridge"
SESSIONS = ROOT / "sessions"
TIMING = ROOT / "timing.jsonl"
CLIENT = ROOT / "client.json"
SAVED = json.loads(CLIENT.read_text()) if CLIENT.exists() else {}
URL = (os.environ.get("HARNESS_BRIDGE_URL") or SAVED.get("url") or "https://dev.webagent.live").rstrip("/")
# Names this process to WebAgent: only the process a call was offered to can take and answer it.
INSTANCE = str(uuid.uuid4())
# WebAgent fails an acknowledged call at 30 minutes; a harness stops at 28 and reports it.
HARNESS_SECS = 28 * 60
# Under the server's 60 s offline window (and over its 25 s hold), so a poll that hangs in the
# network path is retried before a waiting call is declared offline.
POLL_TIMEOUT = 35
PREVIEW = 2000  # characters of a long message shown inline; the full text stays in the saved request
# As close to a plain API call as subscription sign-in allows (--bare needs an API key): no
# CLAUDE.md or settings files, hooks, auto-memory, skills or Chrome, and no MCP server but this
# script's stub for WebAgent's tools. The CLI still adds the account email, working directory,
# date and model identity.
BARE = ["--setting-sources=", "--settings", '{"autoMemoryEnabled":false}', "--strict-mcp-config",
        "--disable-slash-commands", "--no-chrome"]
AUTO_PROMPT = ("You are the language model behind a WebAgent agent. Act exactly as that model: follow the "
    "agent instructions and conversation you are given and decide the next assistant turn. WebAgent's tools are "
    "your mcp__harness__* tools (mcp__harness__<name> for the WebAgent tool <name>). Calling one hands it to "
    "WebAgent, which runs it and sends the result in your next message, so after your tool calls end your turn "
    "without waiting. Images are saved as files you may Read. Your final text is the agent's reply.")
OPENCODE_PROMPT = ("You are the language model behind a WebAgent agent. Act exactly as that model: follow the "
    "agent instructions and conversation you are given and decide the next assistant turn. WebAgent's tools are "
    "your webagent_* tools (webagent_<name> for the WebAgent tool <name>). Calling one hands it to WebAgent, which "
    "runs it and sends the result in your next message, so after your tool calls end your turn without waiting. "
    "To run a command use webagent_shell, never bash; to read or write files use WebAgent's file tools. "
    "Never use your other tools: every use is rejected, and this computer is not the agent's workspace.")
# The installer puts opencode in ~/.opencode/bin, which a systemd PATH usually lacks.
OPENCODE = shutil.which("opencode") or str(Path.home() / ".opencode" / "bin" / "opencode")
OPENCODE_HOME = Path.home() / ".cache" / "opencode-bridge"
MCP_PREFIX = "webagent_"


def opencode_config(work, model, stub=None):
    """Free OpenCode models answer only while the harness lists its own tools, so for them they stay listed
    but "ask" makes the headless run reject every use (never pass --auto). Paid models get "deny", which
    hides them, so the model cannot pick bash over webagent_shell. WebAgent's tools come from this script's
    MCP stub, which records each call for the reply instead of running it; WebAgent's own policy and
    approvals are their only gate. Code Mode is off so the model gets every WebAgent schema as a native
    tool; the skill tool is denied so this computer's skills stay out of the prompt. OpenCode 2.x
    converts the older config layout and drops `codemode`, so this uses the 2.x layout."""
    own = "ask" if model.startswith("opencode/") else "deny"
    return json.dumps({
        "mcp": {"servers": {"webagent": {"type": "local", "codemode": False,
                                         "command": [sys.executable, "-B", str(Path(__file__).resolve()), "mcp", str(stub or work)]}}},
        "agents": {"webagent-bridge": {"mode": "primary", "system": OPENCODE_PROMPT,
                                       "permissions": [{"action": "*", "resource": "*", "effect": own},
                                                       {"action": "skill", "resource": "*", "effect": "deny"},
                                                       {"action": MCP_PREFIX + "*", "resource": "*", "effect": "allow"}]}}})


FORK_SUPPORT = None


def fork_support():
    """Bridge providers whose CLI on this computer can fork a session (Claude Code's --fork-session,
    OpenCode 2.0.32+'s `run --fork`), checked once per process. Only these get fork calls and
    checkpoint sessions; WebAgent summarizes with its System model for the rest."""
    global FORK_SUPPORT
    if FORK_SUPPORT is None:
        def output(command):
            try:
                done = subprocess.run(command, capture_output=True, text=True, timeout=30)
                return done.stdout + done.stderr
            except (OSError, subprocess.TimeoutExpired):
                return ""
        found = []
        if "--fork-session" in output(["claude", "--help"]):
            found.append("claude_bridge")
        version = re.search(r"(\d+)\.(\d+)\.(\d+)", output([OPENCODE, "--version"]))
        if version and tuple(map(int, version.groups())) >= (2, 0, 32) and "--fork" in output([OPENCODE, "run", "--help"]):
            found.append("opencode_bridge")
        FORK_SUPPORT = found
    return FORK_SUPPORT


# Main calls running per WebAgent chat. A fork waits for them, so it copies a settled session.
BUSY, BUSY_LOCK = {}, threading.Condition()


def busy(chat, change):
    with BUSY_LOCK:
        BUSY[chat] = BUSY.get(chat, 0) + change
        if not BUSY[chat]:
            del BUSY[chat]
            BUSY_LOCK.notify_all()


def wait_idle(chat):
    with BUSY_LOCK:
        if not BUSY_LOCK.wait_for(lambda: chat not in BUSY, timeout=HARNESS_SECS):
            raise RuntimeError("the chat's own call did not finish in time to fork its session")


# Client tunables. The server sends the live values (`models-runtime.json`, harness_bridge.client)
# with every poll; these defaults apply until then and with an older server.
HEARTBEAT_SECS = 20
SERVER_IDLE_SECS = 15 * 60
STOP_GRACE_SECS = 5
SETTINGS = {"heartbeat_seconds": "HEARTBEAT_SECS", "stall_seconds": "STALL_SECS", "probe_seconds": "PROBE_SECS",
            "server_idle_seconds": "SERVER_IDLE_SECS", "stop_grace_seconds": "STOP_GRACE_SECS",
            "harness_seconds": "HARNESS_SECS"}


def apply_settings(settings):
    for key, name in SETTINGS.items():
        value = (settings or {}).get(key)
        if isinstance(value, int) and value > 0:
            globals()[name] = value


# One `opencode serve` per chat folder and permission mode, kept warm between calls. A fresh
# `opencode run --standalone` starts the model before its MCP stub connects, so every call's first
# step failed with "Unknown tool" and was repeated (35-80 s each on a long context). Each server is
# its own systemd user unit where available, so it outlives a restart of this client, which then
# reattaches to it through REGISTRY; its password is kept beside it for the same reason.
SERVERS, SERVERS_LOCK = {}, threading.Lock()
REGISTRY = ROOT / "servers.json"
PASSWORD_FILE = ROOT / "server-password"


def server_password():
    """The warm servers' password, created on first use (never on import, so tests and source
    checkouts get no file)."""
    global SERVER_PASSWORD
    if SERVER_PASSWORD:
        return SERVER_PASSWORD
    try:
        SERVER_PASSWORD = PASSWORD_FILE.read_text().strip()
        return SERVER_PASSWORD
    except OSError:
        password = secrets.token_hex(24)
        try:
            ROOT.mkdir(parents=True, exist_ok=True)
            fd = os.open(PASSWORD_FILE, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w") as file:
                file.write(password)
        except OSError:
            pass  # this process still works; servers simply cannot be reattached later
        SERVER_PASSWORD = password
        return password


SERVER_PASSWORD = None


class Unit:
    """A server running as its own systemd user unit."""

    def __init__(self, name):
        self.name = name

    def alive(self):
        return subprocess.run(["systemctl", "--user", "is-active", "--quiet", self.name]).returncode == 0

    def stop(self):
        subprocess.run(["systemctl", "--user", "stop", self.name], capture_output=True)


class Child:
    """A server running as a child process (no systemd user session)."""

    def __init__(self, process):
        self.process, self.name = process, None

    def alive(self):
        return self.process.poll() is None

    def stop(self):
        stop_server(self.process)


def server_request(url, path, work, timeout=5):
    query = urllib.parse.urlencode({"location[directory]": str(work)})
    auth = base64.b64encode(f"opencode:{server_password()}".encode()).decode()
    request = urllib.request.Request(f"{url}{path}?{query}", headers={"Authorization": "Basic " + auth})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.load(response)


def stub_connected(url, work):
    """True once the chat folder's WebAgent MCP stub is connected on this server."""
    try:
        servers = server_request(url, "/api/mcp", work)["data"]
    except (OSError, ValueError, KeyError):
        return False
    return any(s.get("name") == "webagent" and (s.get("status") or {}).get("status") == "connected" for s in servers)


def start_server(work, model, port, stub=None):
    command = [OPENCODE, "serve", "--hostname", "127.0.0.1", "--port", str(port)]
    env = {"OPENCODE_CONFIG_CONTENT": opencode_config(work, model, stub), "OPENCODE_SERVER_PASSWORD": server_password()}
    if shutil.which("systemd-run"):
        name = "webagent-opencode-" + hashlib.sha256(str(stub or work).encode()).hexdigest()[:12] + f"-{port}"
        started = subprocess.run(["systemd-run", "--user", "--quiet", "--collect", f"--unit={name}",
                                  f"--working-directory={work}", "--property=KillMode=control-group",
                                  *[f"--setenv={k}={v}" for k, v in env.items()], *command],
                                 capture_output=True, text=True)
        if started.returncode == 0:
            return Unit(name)
    return Child(subprocess.Popen(command, cwd=work, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL, start_new_session=True, env={**os.environ, **env}))


def save_registry():
    """Record the unit servers so a restarted client can reattach to them. Caller holds SERVERS_LOCK."""
    rows = {"\0".join(key): {"unit": s["handle"].name, "url": s["url"], "tools": s["tools"], "used": s["used"]}
            for key, s in SERVERS.items() if s["handle"].name}
    try:
        temp = REGISTRY.with_suffix(".tmp")
        temp.write_text(json.dumps(rows))
        temp.replace(REGISTRY)
    except OSError:
        pass


def reattach_servers():
    """Adopt servers a previous run of this client left running; stop any that are not healthy."""
    try:
        rows = json.loads(REGISTRY.read_text())
    except (OSError, ValueError):
        return
    with SERVERS_LOCK:
        for key, row in rows.items():
            work, own = key.split("\0")
            unit = Unit(row["unit"])
            if unit.alive() and stub_connected(row["url"], work):
                SERVERS[(work, own)] = {"handle": unit, "url": row["url"], "tools": row["tools"], "used": time.time()}
            else:
                unit.stop()
        save_registry()


def opencode_server(work, model, stub=None):
    """URL of a ready server for this chat folder, started on first use and returned only once its
    MCP stub is connected, so the first step already has WebAgent's tools. The stub lists tools.json as
    it was at start, so a changed tool set restarts the server. A fork server (`stub`, its own folder for
    tools.json and calls.jsonl) still runs in the chat folder: OpenCode puts the working folder in its
    system prompt, so only then does a fork's request start like the chat's and reuse its cache."""
    own = "ask" if model.startswith("opencode/") else "deny"
    key = (str(work), own + (":fork" if stub else ""))
    tools = hashlib.sha256(((stub or work) / "tools.json").read_bytes()).hexdigest()
    with SERVERS_LOCK:
        server = SERVERS.get(key)
        if server and server["tools"] == tools and server["handle"].alive():
            server["used"] = time.time()
            return server["url"]
        if server:
            server["handle"].stop()
            del SERVERS[key]
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        handle = start_server(work, model, port, stub)
        url = f"http://127.0.0.1:{port}"
        deadline = time.time() + 30
        while not stub_connected(url, work):
            if not handle.alive() or time.time() > deadline:
                handle.stop()
                raise RuntimeError("opencode server did not become ready")
            time.sleep(0.2)
        # opencode fetches the stub's tool list when its first session starts, too late for that
        # session's first step; one tiny warm-up prompt makes it fetch them before the real call.
        warm_up(url, work, model, stub)
        SERVERS[key] = {"handle": handle, "url": url, "used": time.time(), "tools": tools}
        save_registry()
        return url


def warm_up(url, work, model, stub=None):
    """One-word prompt on a new server; its result does not matter. It runs through opencode_turn, so
    a failing provider ends it at opencode's first retry instead of after PROBE_SECS of silent retries;
    the real call then reports that failure."""
    command = [OPENCODE, "run", "--server", url, "--agent", "webagent-bridge", "-m", model, "--format", "json"]
    try:
        opencode_turn(command, "Reply with the single word OK. Do not use tools.", work, model, stub)
    except (HarnessFailure, RuntimeError, Stopped):
        pass


def stop_server(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass


def reap_servers():
    """Stop servers idle past SERVER_IDLE_SECS; a call in progress keeps its server's `used` fresh."""
    while True:
        time.sleep(60)
        with SERVERS_LOCK:
            for key, server in list(SERVERS.items()):
                if time.time() - server["used"] > SERVER_IDLE_SECS or not server["handle"].alive():
                    server["handle"].stop()
                    del SERVERS[key]
            save_registry()


def secret():
    if not SAVED.get("secret"):
        sys.exit("not paired: run `harness-bridge.py pair URL CODE` with the code from the Models page")
    return SAVED["secret"]


class Transient(Exception):
    """A gateway or network failure worth retrying."""


class Gone(Exception):
    """The call is no longer this process's: offered elsewhere, finished or revoked."""


def post(path, body, timeout=60, url=None):
    headers = {"Content-Type": "application/json", "User-Agent": "harness-bridge/2"}
    if url is None:
        headers["Authorization"] = "Bearer " + secret()
    # A manual `reply` answers as the process that fetched the call (saved with it).
    request = urllib.request.Request((url or URL) + path, json.dumps({"instance": INSTANCE, **body}).encode(),
                                     method="POST", headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code in (502, 503, 504):
            raise Transient(f"HTTP {error.code}")
        detail = error.read()[:300].decode(errors="replace")
        if error.code in (404, 409):
            raise Gone(f"HTTP {error.code} {detail}")
        sys.exit(f"{path}: HTTP {error.code} {detail}")
    except (urllib.error.URLError, TimeoutError, ConnectionError) as error:
        raise Transient(str(error))


# Harness processes by call id, so a Stop from WebAgent can interrupt the one running that call.
RUNNING, STOPPED, CURRENT = {}, set(), threading.local()
# Calls WebAgent no longer holds (its heartbeat answered 404): their failures are not reported.
GONE = set()


class HarnessFailure(RuntimeError):
    """A failure WebAgent should file by where it happened: "upstream" is the harness's own provider
    (overloaded, quota, auth, not responding); "harness" is this client or the harness process."""

    def __init__(self, detail, source="harness", category=None, retry_after=None):
        super().__init__(detail)
        self.source, self.category, self.retry_after = source, category, retry_after


UPSTREAM = re.compile(r"Upstream request failed: (?:\[(?P<code>[\w.-]+)\] )?(?P<text>.*)|\[(?P<bare>[\w.-]+)\] ", re.S)


def classify(error):
    """(category, retry_after) when an error came from the harness's provider, else None. Takes
    opencode's error object ({"type": "provider.…", "status": 503, "message": …}) or its message."""
    structured = isinstance(error, dict)
    message = str(error.get("message", "")) if structured else str(error)
    match = UPSTREAM.search(message)
    lowered = message.lower()
    provider = structured and (str(error.get("type", "")).startswith("provider") or "status" in error)
    if not (match or provider) and not any(k in lowered for k in ("usage limit", "insufficient account funds")):
        return None
    words = " ".join(filter(None, [str(error.get("type", "")) if structured else "", lowered]))
    wait = re.search(r"after (\d+) ?s", words)
    status = error.get("status") if structured else None
    if status in (429, 503, 529) or any(
            k in words for k in ("overload", "capacity", "cap_refused", "rate limit", "rate_limit", "retry shortly")):
        category = "overloaded"
    elif any(k in words for k in ("fund", "usage limit", "quota", "billing")):
        category = "quota"
    elif any(k in words for k in ("auth", "api key", "unauthorized", "forbidden")):
        category = "auth"
    else:
        category = "upstream"
    return category, int(wait[1]) if wait else None


def harness_failure(errors, returncode, stderr):
    messages = [str(e.get("message", e)) if isinstance(e, dict) else str(e) for e in errors]
    detail = f"opencode failed ({returncode}): {messages or stderr[-300:]}"
    upstream = next(filter(None, map(classify, errors)), None)
    return HarnessFailure(detail, "upstream", *upstream) if upstream else HarnessFailure(detail)


class Stopped(Exception):
    """WebAgent stopped the call; it wants no reply."""


def interrupt(process, grace=None):
    """Ctrl-C the harness, as its own stop key would; kill it if it is still running after the grace period."""
    def signal_group(sig):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
    if process.poll() is None:
        signal_group(signal.SIGINT)
        threading.Timer(grace or STOP_GRACE_SECS, lambda: process.poll() is None and signal_group(signal.SIGKILL)).start()


def start_harness(command, **options):
    """Start a harness process in its own process group, registered under the call this thread answers."""
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True, **options)
    call = getattr(CURRENT, "call", None)
    if call:
        RUNNING[call] = process
        if call in STOPPED:
            interrupt(process)
    return process


def stop_calls(ids):
    for call in ids:
        STOPPED.add(call)
        if call in RUNNING:
            print(f"{call}: stopped by WebAgent; interrupting its harness", file=sys.stderr)
            interrupt(RUNNING[call])


def fetch(session=None, forks=()):
    """Long-poll until a call arrives and this process takes it; ride out gateway restarts and network blips.
    An unacknowledged call returns to WebAgent's queue after a minute, so a lost response or ack only delays it.
    `forks` names the providers this process can fork sessions for; only those get fork calls."""
    request = {"session": session} if session else {}
    if forks:
        request["fork"] = list(forks)
    while True:
        try:
            response = post("/api/v3/harness-bridge/next", request, timeout=POLL_TIMEOUT)
            stop_calls(response.get("cancelled") or [])
            apply_settings(response.get("settings"))
            call = response["call"]
            if call:
                post("/api/v3/harness-bridge/ack", {"id": call["id"]})
        except (Transient, Gone) as error:
            print(f"poll failed ({error}); retrying in 2 s", file=sys.stderr)
            time.sleep(2)
            continue
        if call:
            call["fetched_at"] = time.time()
            call["instance"] = INSTANCE
            return call


def cmd_pair(url, code, name):
    reply = post("/api/v3/harness-bridge/pair", {"code": code, "name": name}, url=url.rstrip("/"))
    ROOT.mkdir(parents=True, exist_ok=True)
    fd = os.open(CLIENT, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as file:
        json.dump({"url": url.rstrip("/"), "client_id": reply["client_id"], "secret": reply["secret"]}, file)
    print(f"paired as client {reply['client_id']}; run `harness-bridge.py auto` to answer calls")


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def text_of(content, folder):
    """Render message content; images become saved files."""
    if isinstance(content, str) or content is None:
        return content or ""
    parts = []
    for part in content:
        if part.get("type") == "text":
            parts.append(part.get("text", ""))
        elif part.get("type") == "image_url":
            url = part["image_url"]["url"] if isinstance(part.get("image_url"), dict) else part.get("image_url", "")
            if url.startswith("data:") and ";base64," in url:
                kind, data = url[5:].split(";base64,", 1)
                raw = base64.b64decode(data)
                path = folder / f"image-{hashlib.sha256(raw).hexdigest()[:12]}.{kind.split('/')[-1]}"
                path.write_bytes(raw)
                parts.append(f"[image: {path}]")
            else:
                parts.append(f"[image: {url[:200]}]")
        else:
            parts.append(json.dumps(part)[:PREVIEW])
    return "\n".join(parts)


def render(message, folder, full=False):
    role = message.get("role")
    body = text_of(message.get("content"), folder)
    if not full and len(body) > PREVIEW:
        body = body[:PREVIEW] + f"\n… [{len(body) - PREVIEW} more characters in the saved request]"
    calls = "".join(f"\n  → {c['function']['name']}({c['function']['arguments']})" for c in message.get("tool_calls") or [])
    label = f"tool result {message.get('tool_call_id', '')}" if role == "tool" else role
    return f"[{label}]\n{body}{calls}"


def tools_of(body):
    return {t["function"]["name"]: t for t in body.get("tools") or [] if t.get("type") == "function"}


def split(body):
    """Leading system/developer messages are the instructions; the rest is the conversation."""
    messages = body.get("messages") or []
    count = next((i for i, m in enumerate(messages) if m.get("role") not in ("system", "developer")), len(messages))
    return messages[:count], messages[count:]


def step_notes(conversation):
    """WebAgent ends a call with its call-scoped notes (mode, publish guidance) as system/developer
    messages after the history; they are new each call and never part of it. Returns the history
    and those notes, so the notes never make the next call look like a rewrite."""
    count = len(conversation)
    while count and conversation[count - 1].get("role") in ("system", "developer"):
        count -= 1
    return conversation[:count], conversation[count:]


def remember(call):
    """Save the full request and work out what is new for this chat."""
    folder = SESSIONS / (call.get("session") or "unknown")
    folder.mkdir(parents=True, exist_ok=True)
    state = load_state(folder)
    (folder / "requests").mkdir(exist_ok=True)
    count = len(list((folder / "requests").iterdir())) + 1
    (folder / "requests" / f"{count:04d}-{call['id']}.json").write_text(json.dumps(call, indent=1))
    # Only the newest few copies are kept (a debugging record; Muse reads the current one).
    keep = max(1, int(cleanup_settings()["keep_requests_per_chat"]))
    for old in sorted((folder / "requests").glob("*.json"), key=lambda p: p.stat().st_mtime)[:-keep]:
        old.unlink(missing_ok=True)
    return folder, state, changes(folder, state, call["body"], count)


def load_state(folder):
    state_path = folder / "state.json"
    return json.loads(state_path.read_text()) if state_path.exists() else {}


def fork_delta(call):
    """What a fork appends to the chat's session, once the chat's own calls are done. The chat's saved
    state is read, never written: the session it describes stays the chat's."""
    wait_idle(call.get("session"))
    check_stopped()
    folder = SESSIONS / (call.get("session") or "unknown")
    state = load_state(folder)
    return folder, state, changes(folder, dict(state), call["body"], 0)


def fork_plan(state, delta, body, session):
    """Whether a fork call can be answered from a fork of the chat's harness session, and with what delta.
    A background summary is planned at one step, but the chat's own calls go on while it waits: by the
    time the fork runs, the session may already hold later steps. When the fork's history (everything
    but its final summary request) is the start of what the session holds, the fork still works; the
    prompt then names where the planned summary ends, so later steps stay out of it. Refusals name
    their reason so they can be told apart."""
    if not session:
        raise RuntimeError("the chat's session cannot be forked: no harness session yet")
    if delta["instructions"] is not None:
        raise RuntimeError("the chat's session cannot be forked: the instructions changed")
    if delta["tools_changed"]:
        raise RuntimeError("the chat's session cannot be forked: the tools changed")
    if not delta["rewritten"]:
        return delta
    _, conversation = split(body)
    conversation, _ = step_notes(conversation)
    seen, digests = delta["previous"], delta["digests"]
    history = len(digests) - 1
    if history < 1 or len(seen) <= history or digests[:history] != seen[:history]:
        raise RuntimeError("the chat's session cannot be forked: its history differs from this request")
    behind = len(seen) - history
    item = conversation[history - 1]
    content = item.get("content")
    last = content if isinstance(content, str) else json.dumps(content or item.get("tool_calls") or "")
    marker = {"role": "user", "content": (
        f"(Your session continued for {behind} more item(s) after the point this summary covers. Summarize "
        "only the conversation up to and including the item that reads: \"" + " ".join(str(last).split())[:200]
        + "\". Leave everything after it out of the summary.)")}
    return {**delta, "rewritten": False, "new": [marker, conversation[-1]]}


def changes(folder, state, body, count):
    """What is new in `body` since the call `state` records; `state` is updated to this call."""
    instructions, conversation = split(body)
    conversation, notes = step_notes(conversation)
    tools = tools_of(body)
    seen = state.get("conversation", [])
    old_schemas = state.get("tool_schemas", {})
    digests = [digest(m) for m in conversation]
    append = len(seen) <= len(conversation) and all(d == old for d, old in zip(digests, seen))
    delta = {
        "folder": folder, "count": count,
        "instructions": None if digest(instructions) == state.get("instructions") else instructions,
        "tools_added": sorted(set(tools) - set(state.get("tools", []))),
        "tools_removed": sorted(set(state.get("tools", [])) - set(tools)),
        "tools_changed": digest(body.get("tools")) != state.get("tools_digest"),
        # Schemas new or different since the last call, and the previous instructions: a resumed session
        # gets only these as an update instead of restarting (a restart resends everything uncached).
        "tool_updates": [t["function"] for n, t in tools.items() if old_schemas.get(n) != digest(t)],
        "previous_instructions": state.get("instructions_text"),
        "rewritten": bool(seen) and not append,
        "new": conversation[len(seen):] if append else conversation,
        # The previous call's conversation, for a checkpoint after a rewrite.
        "previous": seen, "digests": digests, "notes": notes,
        # The harness session keeps every prompt it is given, so WebAgent's step guidance goes in only
        # with each new user message and when its text differs from the last copy this session
        # received; other steps of the same turn skip it. A rewrite or a fresh session gets it again.
        "notes_repeated": bool(notes) and bool(seen) and append and digest(notes) == state.get("notes_digest")
                          and not any(m.get("role") == "user" for m in conversation[len(seen):]),
    }
    # A run's closing call ends with a private closer message that WebAgent keeps out of history: the
    # session holds it, but the next call (or a fork) must still read as an append, not a rewrite.
    recorded = digests[:-1] if body.get("webagent_closing") and digests else digests
    state.update(instructions=digest(instructions), tools=sorted(tools), tools_digest=digest(body.get("tools")),
                 instructions_text=[text_of(m.get("content"), folder) for m in instructions],
                 tool_schemas={n: digest(t) for n, t in tools.items()},
                 conversation=recorded)
    # A call without guidance (a run's closing call) leaves the last copy the session has as current.
    if notes:
        state["notes_digest"] = digest(notes)
    return delta


def save_state(folder, state):
    (folder / "state.json").write_text(json.dumps(state, indent=1))


def record(call, state, delta, started, ended, output=None):
    """Append one answered call's timing. gap_s runs from this chat's previous reply to this fetch: WebAgent's
    own work between turns plus both 1 s polls (WebAgent noticing the reply, this client picking up the call)."""
    now = time.time()
    last = state.get("replied_at")
    entry = {"at": round(now, 3), "call": call["id"], "chat": call.get("session"), "model": call["body"].get("model"),
             # A new user message starts a turn after idle time, not a between-turns gap.
             "user_turn": any(m.get("role") == "user" for m in delta["new"]),
             "gap_s": round(call["fetched_at"] - last, 3) if last else None,
             "prep_s": round(started - call["fetched_at"], 3), "cli_s": round(ended - started, 3),
             "reply_s": round(now - ended, 3)}
    if output:
        # duration_api_ms accumulates over a resumed CLI session, so only duration_ms is per call.
        entry["cli_inner_s"] = output.get("duration_ms", 0) / 1000
    with open(TIMING, "a") as out:
        out.write(json.dumps(entry) + "\n")
    state["replied_at"] = now


def cmd_stats(hours):
    cutoff = time.time() - hours * 3600
    rows = [json.loads(line) for line in TIMING.read_text().splitlines()] if TIMING.exists() else []
    rows = [r for r in rows if r["at"] >= cutoff]
    turns = [r for r in rows if not r["user_turn"] and r["gap_s"] is not None]
    print(f"{len(rows)} calls in the last {hours:g} h, {len(turns)} between turns of a run")
    metrics = [("gap_s", turns, "reply -> next call fetched (WebAgent work + both polls)"),
               ("prep_s", rows, "fetched -> CLI started (saving the request)"),
               ("startup_s", [dict(r, startup_s=r["cli_s"] - r["cli_inner_s"]) for r in rows if "cli_inner_s" in r],
                "CLI process start/resume outside its own timer"),
               ("cli_s", rows, "whole CLI run"),
               ("reply_s", rows, "CLI done -> reply accepted")]
    for key, subset, label in metrics:
        values = sorted(r[key] for r in subset if r.get(key) is not None)
        if values:
            pick = lambda q: values[min(len(values) - 1, int(q * len(values)))]
            print(f"{key:10} p50 {pick(.5):7.2f}s  p90 {pick(.9):7.2f}s  total {sum(values):8.1f}s  {label}")


def cmd_next(session):
    call = fetch(session)
    folder, state, delta = remember(call)
    save_state(folder, state)
    body = call["body"]
    print(f"CALL {call['id']}  chat {call.get('session')}  model {body.get('model')}  "
          f"effort {body.get('reasoning_effort', 'default')}  request #{delta['count']}")
    print(f"full request: {folder}/requests/")
    if delta["instructions"] is not None:
        print("\n=== instructions" + (" (changed)" if delta["count"] > 1 else ""))
        for message in delta["instructions"]:
            print(render(message, folder, full=True))
    if delta["tools_added"] or delta["tools_removed"]:
        print(f"\n=== tools  +{delta['tools_added']}  -{delta['tools_removed']}")
    elif delta["tools_changed"]:
        print("\n=== tool schemas changed (same names)")
    print("\n=== context rewritten: full conversation" if delta["rewritten"] else "\n=== new messages")
    for message in delta["new"]:
        print(render(message, folder))


def parse_reply(raw, body):
    """Validate a reply against the call's tools and build the chat-completions message."""
    reply = json.loads(raw)
    tools = tools_of(body)
    calls = []
    for item in reply.get("tool_calls") or []:
        name, arguments = item["name"], item["arguments"]
        if name not in tools:
            sys.exit(f"unknown tool {name!r}; offered: {sorted(tools)}")
        if isinstance(arguments, str):
            arguments = json.loads(arguments)
        calls.append({"id": "call_" + uuid.uuid4().hex[:24], "type": "function",
                      "function": {"name": name, "arguments": json.dumps(arguments)}})
    message = {"role": "assistant", "content": reply.get("content") or ""}
    if calls:
        message["tool_calls"] = calls
    return message


def find_call(call_id):
    for path in SESSIONS.glob(f"*/requests/*-{call_id}.json"):
        return json.loads(path.read_text())
    sys.exit(f"call {call_id} was not fetched by this client")


def cmd_reply(call_id, source):
    raw = sys.stdin.read() if source == "-" else Path(source).read_text()
    call = find_call(call_id)
    print(post("/api/v3/harness-bridge/reply", {"id": call_id, "instance": call.get("instance"),
                                                "message": parse_reply(raw, call["body"])}))


def prompt_for(call, delta, fresh, listed=True):
    """The text the headless session sees: everything on a fresh session, otherwise only what is new.
    A harness that serves WebAgent's tools over MCP already holds every schema; `listed=False` keeps
    them out of the text (only muse, which has no MCP, reads them from the prompt)."""
    folder, body = delta["folder"], call["body"]
    lines = []
    if fresh:
        instructions, conversation = split(body)
        conversation = step_notes(conversation)[0]
        lines.append("## Agent instructions\n" + "\n\n".join(text_of(m.get("content"), folder) for m in instructions))
        if listed:
            lines.append("## Tools you may call\n" + json.dumps([t["function"] for t in tools_of(body).values()], indent=1))
        messages = conversation
    else:
        lines += updates(delta, folder, listed)
        messages = delta["new"]
    lines.append("## Conversation" if fresh else "## New since your last turn")
    lines += [render(m, folder, full=True) for m in messages]
    if delta.get("notes") and (fresh or not delta.get("notes_repeated")):
        # The text starts with the mode's per-turn label (for example "Harness guidance:").
        lines.append("\n\n".join(text_of(m.get("content"), folder) for m in delta["notes"]))
    return "\n\n".join(lines)


def updates(delta, folder, listed=True):
    """On a resumed session: what changed in the instructions and tools, appended rather than restarting."""
    lines = []
    if delta["instructions"] is not None:
        new = "\n\n".join(text_of(m.get("content"), folder) for m in delta["instructions"]).splitlines()
        old = delta["previous_instructions"]
        if old is None:
            lines.append("## Agent instructions (replace the earlier ones)\n" + "\n".join(new))
        else:
            diff = [l for l in difflib.unified_diff("\n\n".join(old).splitlines(), new, lineterm="", n=1)
                    if not l.startswith(("---", "+++"))]
            lines.append("## Agent instructions updated (diff against the earlier ones; + added, - removed)\n"
                         + "\n".join(diff))
    if delta["tools_removed"]:
        lines.append("## Tools no longer available\n" + ", ".join(delta["tools_removed"]))
    if delta["tool_updates"] and listed:
        lines.append("## Tools added or changed\n" + json.dumps(delta["tool_updates"], indent=1))
    return lines


def resumable(state, key, delta):
    """Restart only when the transcript was rewritten (compaction or edits); other changes are appended."""
    return bool(state.get(key)) and not delta["rewritten"]


# Leading messages worth a checkpoint session (about 1000 tokens); shorter ones are simply resent.
CHECKPOINT_MIN_CHARS = 4000
CHECKPOINT_NOTE = ("(Context checkpoint: the conversation continues in the next message. Reply with only OK "
                   "and call no tools.)")


def checkpoint_setup(state, body):
    """What a checkpoint's cached prefix depends on besides its messages."""
    return digest([state.get("instructions"), state.get("tools_digest"), body.get("model"),
                   body.get("reasoning_effort")])


def checkpoint_plan(state, key, delta, body):
    """A rewritten transcript (after a fold) restarts the chat's session. The leading messages it shares
    with the previous call (after a fold, the earlier summary blocks, which later folds keep) go into a
    checkpoint session, and the turn runs in a fork of it, so the next fold forks the checkpoint instead
    of resending them. None starts fresh as before; otherwise fork `base` (None: start one) after adding
    the messages up to `keep` when `extend`."""
    if not delta["rewritten"]:
        return None
    digests, previous = delta["digests"], delta["previous"]
    keep = 0
    while keep < min(len(previous), len(digests) - 1) and previous[keep] == digests[keep]:
        keep += 1
    base = state.get(key)
    held = len(base["messages"]) if base else 0
    if not (base and base.get("setup") == checkpoint_setup(state, body) and held < len(digests)
            and base["messages"] == digests[:held]):
        base, held = None, 0
    if base and held >= keep:
        return {"base": base, "keep": held, "extend": False}
    if len(json.dumps(split(body)[1][held:keep])) < CHECKPOINT_MIN_CHARS:
        return {"base": base, "keep": held, "extend": False} if base else None
    return {"base": base, "keep": keep, "extend": True}


def checkpoint_prompt(call, plan, folder):
    """The text that turns `plan`'s base (or a new session) into the new checkpoint."""
    body, held = call["body"], len(plan["base"]["messages"]) if plan["base"] else 0
    messages = step_notes(split(body)[1])[0][held:plan["keep"]]
    lines = []
    if not plan["base"]:
        instructions = split(body)[0]
        # Checkpoints are claude and opencode sessions, which hold the tools over MCP.
        lines.append("## Agent instructions\n" + "\n\n".join(text_of(m.get("content"), folder) for m in instructions))
        lines.append("## Conversation")
    else:
        lines.append("## Conversation, continued")
    lines += [render(m, folder, full=True) for m in messages]
    lines.append(CHECKPOINT_NOTE)
    return "\n\n".join(lines)


def checkpoint_record(plan, state, delta, body, session):
    return {"session": session, "messages": delta["digests"][:plan["keep"]], "setup": checkpoint_setup(state, body)}


def continued_prompt(call, keep, folder):
    """The turn in a fork of a checkpoint holding the first `keep` messages."""
    messages = split(call["body"])[1][keep:]
    return "\n\n".join(["## Conversation, continued"] + [render(m, folder, full=True) for m in messages])


def mcp_stub(work):
    """Minimal MCP stdio server: lists the call's WebAgent tools and records each call in calls.jsonl."""
    work = Path(work)
    tools = json.loads((work / "tools.json").read_text())
    for line in sys.stdin:
        request = json.loads(line)
        method, rid = request.get("method"), request.get("id")
        if rid is None:
            continue  # notifications need no answer
        if method == "initialize":
            result = {"protocolVersion": request["params"].get("protocolVersion", "2025-06-18"),
                      "capabilities": {"tools": {}}, "serverInfo": {"name": "webagent", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": t["name"], "description": t.get("description", ""),
                                 "inputSchema": t.get("parameters") or {"type": "object"}} for t in tools]}
        elif method == "tools/call":
            entry = json.dumps({"name": request["params"]["name"], "arguments": request["params"].get("arguments") or {}})
            log = work / "calls.jsonl"
            # Models sometimes repeat a call while "waiting" for its result; record it once.
            repeat = log.exists() and entry in log.read_text().splitlines()
            if not repeat:
                with open(log, "a") as out:
                    out.write(entry + "\n")
            result = {"content": [{"type": "text", "text": "Already sent. End your turn now." if repeat else
                                   "Sent to WebAgent; its result arrives in your next message. "
                                   "End your turn now unless you have other tool calls to make."}]}
        elif method == "ping":
            result = {}
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": method}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}), flush=True)


# A step that streams nothing (no reasoning, text or tool events) for this long is checked with a probe.
STALL_SECS = 120
PROBE_SECS = 30
# Healthy probes in a row with the call itself still silent before the call counts as stuck: the
# provider is up but this call is not progressing, so it fails as "stalled" and WebAgent retries it.
SLOW_PROBES = 2


class StallWatch:
    """Stop at opencode's first provider error (`session.retry.scheduled`), and tell a slow model from
    a dead provider. Any session event on the chat's server is progress;
    after STALL_SECS of silence a one-word probe goes to the same model on the same server. A probe
    that answers means the provider is up and the call is just slow, so waiting continues; one that
    fails or times out ends the call now as an upstream failure instead of after minutes."""

    def __init__(self, server, work, model, process):
        self.server, self.work, self.model, self.process = server, work, model, process
        self.last, self.failure, self.closed, self.response = time.time(), None, False, None
        self.probing, self.slow = False, 0
        threading.Thread(target=self.listen, daemon=True).start()
        threading.Thread(target=self.check, daemon=True).start()

    def listen(self):
        url = self.server + "/api/event?directory=" + urllib.parse.quote(str(self.work))
        auth = base64.b64encode(f"opencode:{server_password()}".encode()).decode()
        try:
            self.response = urllib.request.urlopen(urllib.request.Request(url, headers={"Authorization": "Basic " + auth}),
                                                   timeout=HARNESS_SECS)
            for line in self.response:
                if self.closed:
                    break
                if not (line.startswith(b"data:") and b'"type":"session.' in line):
                    continue
                if b'"type":"session.retry.scheduled"' in line:
                    self.provider_failed(json.loads(line[5:]).get("data") or {})
                    return
                # The probe's own session reports on this server too; only the call's events are progress.
                if not self.probing:
                    self.last, self.slow = time.time(), 0
        except Exception:  # closed on purpose, or no stream: the call then runs unwatched
            pass

    def provider_failed(self, data):
        """opencode got an error from its provider and would retry silently with growing gaps (2 s,
        4 s, 8 s, … for minutes). Stop at the first one so WebAgent's own retry policy decides."""
        error = data.get("error") or {}
        upstream = classify(error) or ("upstream", None)
        self.failure = HarnessFailure(f"provider error: {error.get('message', 'unknown')}"[:300], "upstream", *upstream)
        interrupt(self.process)

    def check(self):
        while not self.closed and self.process.poll() is None:
            time.sleep(5)
            if time.time() - self.last < STALL_SECS or self.closed:
                continue
            silent = round(time.time() - self.last)
            self.probing = True
            try:
                error = self.probe()
            finally:
                self.probing = False
            if error is None:
                self.slow += 1
                if self.slow < SLOW_PROBES:
                    self.last = time.time()
                    continue
                error = f"the model answers probes but this call has been silent {self.slow} times running"
            upstream = classify(error) or ("stalled", None)
            self.failure = HarnessFailure(f"no response for {silent} s and a probe failed: {error}"[:300],
                                          "upstream", *upstream)
            interrupt(self.process)
            return

    def probe(self):
        """None when the model answers a one-word prompt in time, else the error."""
        command = [OPENCODE, "run", "--server", self.server, "--agent", "webagent-bridge", "-m", self.model,
                   "--format", "json"]
        try:
            result = subprocess.run(command, input="Reply with the single word OK. Do not use tools.", cwd=self.work,
                                    capture_output=True, text=True, timeout=PROBE_SECS,
                                    env={**os.environ, "OPENCODE_SERVER_PASSWORD": server_password()})
        except subprocess.TimeoutExpired:
            return f"probe got no answer within {PROBE_SECS} s"
        events = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        errors = [str((e.get("error") or {}).get("message", e))[:200] for e in events if e.get("type") == "error"]
        if errors or not any(e.get("type") == "text" for e in events):
            return " ".join(errors) or f"probe failed ({result.returncode})"
        return None

    def close(self):
        self.closed = True
        if self.response:
            try:
                self.response.close()
            except OSError:
                pass


# OpenCode's own server sometimes drops a connection ("socket connection was closed unexpectedly"); the
# next `opencode run` on it then fails writing its prompt (broken pipe). One retry gets a fresh connection.
DROPPED = ("socket connection was closed unexpectedly", "Broken pipe")


def opencode_turn(command, prompt, work, model, stub=None):
    """`opencode_turn_once`, run again once when OpenCode dropped its connection before any tool call was
    recorded (the stub only records calls; WebAgent runs them, so a retry repeats no effect)."""
    try:
        return opencode_turn_once(command, prompt, work, model, stub)
    except (BrokenPipeError, RuntimeError) as error:
        calls = (stub or work) / "calls.jsonl"
        recorded = calls.exists() and calls.stat().st_size
        if recorded or not any(text in str(error) or isinstance(error, BrokenPipeError) and text == "Broken pipe"
                               for text in DROPPED):
            raise
        print(f"opencode dropped its connection ({str(error)[:120]}); restarting its server and retrying once",
              file=sys.stderr)
        # The server process can stay alive with its connection gone, so it is replaced, not reused.
        if "--server" in command:
            at = command.index("--server") + 1
            with SERVERS_LOCK:
                for key, server in list(SERVERS.items()):
                    if server["url"] == command[at]:
                        server["handle"].stop()
                        del SERVERS[key]
            command = command[:at] + [opencode_server(work, model, stub)] + command[at + 1:]
        return opencode_turn_once(command, prompt, work, model, stub)


def opencode_turn_once(command, prompt, work, model, stub=None):
    """Run one `opencode run`; return its session id, the final text and token usage.

    The run stops at the end of the first step that called a WebAgent tool: the stub only records the call,
    so any further step is the model winding down (30-45 s each on a long context). The next call resumes
    the session with the tool's real result."""
    # The prompt goes on stdin: a fresh session's full history can exceed the 128 KiB argument cap.
    # `opencode run` takes its directory from PWD, not the process cwd; without it every chat is /home/ar.
    started_ms = time.time() * 1000
    process = start_harness(command, cwd=work, env={**os.environ, "PWD": str(work), "OPENCODE_SERVER_PASSWORD": server_password()})
    stop = lambda: interrupt(process)
    timer = threading.Timer(HARNESS_SECS, stop)
    timer.start()
    stderr = []
    threading.Thread(target=lambda: stderr.append(process.stderr.read()), daemon=True).start()
    process.stdin.write(prompt)
    process.stdin.close()
    server = command[command.index("--server") + 1] if "--server" in command else None
    watch = StallWatch(server, work, model, process) if server else None
    session, texts, usage, errors, stopped = None, {}, {"input": 0, "output": 0, "cached": 0}, [], False
    try:
        for line in process.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            session = event.get("sessionID") or session
            part = event.get("part") or {}
            if event.get("type") == "text":
                texts.setdefault(part.get("messageID"), []).append(part.get("text", ""))
            elif event.get("type") == "step_finish":
                tokens = part.get("tokens") or {}
                cache = tokens.get("cache") or {}
                # Each step is one request; the last one's prompt is the size WebAgent's request
                # reached (earlier steps repeat it), so prompt counts are kept, not summed.
                usage["input"] = tokens.get("input", 0) + cache.get("read", 0) + cache.get("write", 0)
                usage["output"] += tokens.get("output", 0)
                usage["cached"] = cache.get("read", 0)
                usage["cache_write"] = cache.get("write", 0)
                if ((stub or work) / "calls.jsonl").exists() and ((stub or work) / "calls.jsonl").stat().st_size:
                    stopped = True
                    stop()
                    break
            elif event.get("type") == "error":
                errors.append(event.get("error") or {"message": str(event)[:300]})
        process.wait()
    finally:
        timer.cancel()
        stop()
        if watch:
            watch.close()
    check_stopped()
    if watch and watch.failure:
        raise watch.failure
    if errors or not session or (process.returncode and not stopped):
        raise harness_failure(errors, process.returncode, "".join(stderr))
    if server and not (usage["input"] or usage["output"]):
        usage = session_usage(server, session, work, started_ms) or usage
    # Every assistant message of the turn reaches WebAgent: a sentence written before the tool
    # calls is the agent's progress note.
    return session, "\n\n".join("".join(parts) for parts in texts.values() if "".join(parts).strip()), usage


def session_usage(server, session, work, since_ms):
    """Token usage of the session's model replies since `since_ms`, read from its server. `opencode run
    --format json` often prints no step_finish event (2.0.32 printed only text), which lost most calls'
    usage. The newest reply gives the prompt and cache counts (earlier steps repeat it); outputs are summed."""
    try:
        messages = server_request(server, "/api/session/" + urllib.parse.quote(session) + "/message", work)["data"]
    except (OSError, ValueError, KeyError, TypeError):
        return None
    return steps_usage(messages, since_ms)


def steps_usage(messages, since_ms):
    """Usage in `session_usage`'s shape from server messages listed newest first; None when none qualify."""
    steps = [m["tokens"] for m in messages
             if isinstance(m.get("tokens"), dict) and (m.get("time") or {}).get("created", 0) >= since_ms]
    if not steps:
        return None
    cache = steps[0].get("cache") or {}
    return {"input": steps[0].get("input", 0) + cache.get("read", 0) + cache.get("write", 0),
            "output": sum(step.get("output", 0) for step in steps),
            "cached": cache.get("read", 0), "cache_write": cache.get("write", 0)}


def opencode_usage(usage):
    """Chat-completions usage for the `opencode run` that answered the call (its last step's prompt; checkpoint
    runs are not the call's request); None when it reported none (some models report no step usage). Cache
    reads are reported apart so WebAgent records them."""
    total = usage
    if not (total["input"] or total["output"]):
        return None
    return {"prompt_tokens": total["input"], "completion_tokens": total["output"],
            "total_tokens": total["input"] + total["output"],
            "prompt_tokens_details": {"cached_tokens": total["cached"], "cache_write_tokens": total.get("cache_write", 0)}}


def opencode_calls(work):
    calls = [json.loads(line) for line in (work / "calls.jsonl").read_text().splitlines()] \
        if (work / "calls.jsonl").exists() else []
    return [{"name": c["name"].removeprefix(MCP_PREFIX), "arguments": c["arguments"]} for c in calls]


def with_images(command, prompt):
    for image in re.findall(r"\[image: (/[^\]]+)\]", prompt):
        command = command + ["-f", image]
    return command


def answer_opencode(call, folder, state, delta):
    body = call["body"]
    model = body.get("model", "")
    fresh = not resumable(state, "opencode_session", delta)
    prompt = prompt_for(call, delta, fresh, listed=False)
    work = OPENCODE_HOME / folder.name
    work.mkdir(parents=True, exist_ok=True)
    (work / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
    run = [OPENCODE, "run", "--server", opencode_server(work, model), "--agent", "webagent-bridge",
           "-m", model, "--format", "json"]
    command = run + ([] if fresh else ["-s", state["opencode_session"]])
    plan = checkpoint_plan(state, "opencode_checkpoint", delta, body) \
        if fresh and "opencode_bridge" in fork_support() else None
    # The work folder is outside this repository: OpenCode would otherwise load the enclosing git project.
    (work / "calls.jsonl").unlink(missing_ok=True)
    started = time.time()
    if plan:
        try:
            base = plan["base"]
            if plan["extend"]:
                text = checkpoint_prompt(call, plan, folder)
                made, _, _ = opencode_turn(
                    with_images(run + (["-s", base["session"], "--fork"] if base else []), text), text, work, model)
                base = state["opencode_checkpoint"] = checkpoint_record(plan, state, delta, body, made)
                (work / "calls.jsonl").unlink(missing_ok=True)
            command = run + ["-s", base["session"], "--fork"]
            prompt = continued_prompt(call, len(base["messages"]), folder)
        except (HarnessFailure, RuntimeError) as error:
            # A checkpoint is an optimization: without one the turn starts fresh as before.
            print(f"{call['id']}: checkpoint skipped ({str(error)[:200]})", file=sys.stderr)
            state.pop("opencode_checkpoint", None)
            (work / "calls.jsonl").unlink(missing_ok=True)
            plan = None
    try:
        session, text, usage = opencode_turn(with_images(command, prompt), prompt, work, model)
    except (HarnessFailure, RuntimeError) as error:
        if not plan or getattr(error, "source", None) == "upstream":
            raise
        # The checkpoint could not be forked (for example its session is gone): start fresh once.
        print(f"{call['id']}: checkpoint fork failed ({str(error)[:200]}); starting fresh", file=sys.stderr)
        state.pop("opencode_checkpoint", None)
        (work / "calls.jsonl").unlink(missing_ok=True)
        prompt = prompt_for(call, delta, True, listed=False)
        session, text, usage = opencode_turn(with_images(run, prompt), prompt, work, model)
    ended = time.time()
    calls = opencode_calls(work)
    message = parse_reply(json.dumps({"content": text, "tool_calls": calls}), body)
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": opencode_usage(usage)})
    record(call, state, delta, started, ended)
    replaced = state.get("opencode_session")
    state["opencode_session"] = session
    save_state(folder, state)
    # A rewrite or checkpoint gave the chat a new main session; the old one is no longer used.
    keep = {session, (state.get("opencode_checkpoint") or {}).get("session")}
    if replaced and replaced not in keep:
        delete_opencode_session(replaced, work, run[run.index("--server") + 1])
    print(f"{call['id']}: answered chat {call.get('session')} with {body.get('model')} "
          f"({'fresh' if fresh else 'resumed'} session {session}, {len(calls)} tool call(s))")


def opencode_session_exists(server, work, session):
    """Whether `session` is visible on `server`; `run -s` would otherwise start an empty session of that id."""
    try:
        return session in json.dumps(server_request(server, "/api/session/" + urllib.parse.quote(session), work))
    except (OSError, ValueError):
        return False


def fork_opencode(call):
    """Answer a fork call from a fork of the chat's OpenCode session, on the chat's own fork server: it runs
    in the chat folder, so the fork's request starts like the chat's and reads its cache, but keeps its
    tool files in its own stub folder, so the chat's server, session, state and tool files are left as
    they are."""
    folder, state, delta = fork_delta(call)
    body = call["body"]
    model = body.get("model", "")
    session = state.get("opencode_session")
    delta = fork_plan(state, delta, body, session)
    work = OPENCODE_HOME / folder.name
    stub = OPENCODE_HOME / (folder.name + ".fork")
    work.mkdir(parents=True, exist_ok=True)
    stub.mkdir(parents=True, exist_ok=True)
    (stub / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
    (stub / "calls.jsonl").unlink(missing_ok=True)
    server = opencode_server(work, model, stub)
    if not opencode_session_exists(server, work, session):
        raise RuntimeError("the chat's OpenCode session is not visible to its fork server")
    prompt = prompt_for(call, delta, False, listed=False)
    command = [OPENCODE, "run", "--server", server, "--agent", "webagent-bridge", "-m", model, "--format", "json",
               "-s", session, "--fork"]
    forked, text, usage = opencode_turn(with_images(command, prompt), prompt, work, model, stub)
    calls = opencode_calls(stub)
    message = parse_reply(json.dumps({"content": text, "tool_calls": calls}), body)
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": opencode_usage(usage)})
    print(f"{call['id']}: answered a fork of chat {call.get('session')} (session {session} -> {forked})")
    # WebAgent accepted the summary; the fork is spent.
    if forked and forked != session:
        delete_opencode_session(forked, work, server)


def check_stopped():
    if getattr(CURRENT, "call", None) in STOPPED:
        raise Stopped()


def heartbeat(call, done):
    """Tell WebAgent this call is still running, now and every HEARTBEAT_SECS. A server without the
    endpoint, or one that no longer holds the call, answers 404 and ends the beating."""
    while not done.is_set():
        try:
            post("/api/v3/harness-bridge/progress", {"id": call})
        except Gone:
            GONE.add(call)
            return
        except Transient:
            pass
        done.wait(HEARTBEAT_SECS)


def answer(call):
    """Run the call's harness; on any failure tell WebAgent at once, so the run retries instead of waiting."""
    CURRENT.call = call["id"]
    done = threading.Event()
    threading.Thread(target=heartbeat, args=(call["id"], done), daemon=True).start()
    try:
        if call.get("fork"):
            forker = FORKERS.get(call.get("provider"))
            if not forker or call.get("provider") not in fork_support():
                raise RuntimeError(f"no session fork for {call.get('provider')} on this client")
            forker(call)
            return
        harness = HARNESSES.get(call.get("provider"))
        if not harness:
            raise RuntimeError(f"no harness for {call.get('provider')} on this client")
        folder, state, delta = remember(call)
        busy(call.get("session"), 1)
        try:
            harness(call, folder, state, delta)
        finally:
            busy(call.get("session"), -1)
    except Stopped:
        print(f"{call['id']}: stopped; no reply sent", file=sys.stderr)
    except (Exception, SystemExit) as error:
        detail = str(error)[:300] or type(error).__name__
        print(f"{call['id']}: {detail}", file=sys.stderr)
        source = {"source": error.source, "category": error.category, "retry_after": error.retry_after} \
            if isinstance(error, HarnessFailure) else {"source": "harness"}
        if call["id"] in GONE:
            # WebAgent already gave up on this call (its own retry is running); nothing to report.
            print(f"{call['id']}: WebAgent no longer waits for this call; failure not reported", file=sys.stderr)
        else:
            try:
                post("/api/v3/harness-bridge/reply", {"id": call["id"], "error": detail, **source})
            except Gone:
                print(f"{call['id']}: WebAgent no longer waits for this call; failure not reported", file=sys.stderr)
            except Transient as failure:
                print(f"{call['id']}: could not report the failure ({failure})", file=sys.stderr)
    finally:
        done.set()
        RUNNING.pop(call["id"], None)
        STOPPED.discard(call["id"])
        GONE.discard(call["id"])


def claude_command(body, stub):
    """`claude -p` for one call. The MCP stub serves the tools in stub/tools.json and records calls in
    stub/calls.jsonl; checkpoints and forks pass the same arguments, so the cached prefix matches."""
    # The CLI reserves the server name "webagent" and silently skips it.
    mcp = {"mcpServers": {"harness": {"command": sys.executable,
                                      "args": ["-B", str(Path(__file__).resolve()), "mcp", str(stub)]}}}
    command = ["claude", "-p", "--output-format", "json", "--system-prompt", AUTO_PROMPT, "--tools", "Read",
               "--mcp-config", json.dumps(mcp), "--allowedTools", "mcp__harness", "--model", body.get("model", ""),
               *BARE]
    if body.get("reasoning_effort"):
        command += ["--effort", body["reasoning_effort"]]
    return command


def run_claude(command, prompt, folder):
    """Run one `claude -p` in the chat folder (its sessions live under that project); return its JSON output."""
    # Prompt on stdin: a fresh chat's full history exceeds Linux's 128 KiB single-argument limit.
    started = time.time()
    process = start_harness(command, cwd=folder)
    try:
        stdout, stderr = process.communicate(prompt, timeout=HARNESS_SECS)
    except subprocess.TimeoutExpired:
        interrupt(process)
        stdout, stderr = process.communicate()
    check_stopped()
    try:
        output = json.loads(stdout)
    except ValueError:
        raise RuntimeError(f"claude failed ({process.returncode}): {stderr[:300]}")
    if output.get("is_error"):
        raise RuntimeError(f"claude error: {str(output.get('result'))[:300]}")
    output["_started"] = started
    return output


def claude_session_usage(folder, session, since, project=None):
    """Exact usage of the requests a `claude -p` run made since `since`, from Claude Code's session log:
    the newest request's prompt and cache counts (earlier ones repeat its prefix), outputs summed. The log
    repeats a request once per content block, so requests are told apart by message id; None when the log
    is missing or holds no counted request."""
    try:
        lines = ((project or claude_project(folder)) / f"{session}.jsonl").read_text().splitlines()
    except OSError:
        return None
    requests = {}
    for line in lines:
        try:
            entry = json.loads(line)
            at = calendar.timegm(time.strptime(entry["timestamp"][:19], "%Y-%m-%dT%H:%M:%S"))
        except (ValueError, KeyError, TypeError):
            continue
        message = entry.get("message") or {}
        usage = message.get("usage") or {}
        counted = any(usage.get(key) for key in ("input_tokens", "cache_read_input_tokens",
                                                 "cache_creation_input_tokens", "output_tokens"))
        if entry.get("type") == "assistant" and message.get("id") and counted and at >= since - 1:
            requests[message["id"]] = usage
    if not requests:
        return None
    newest = list(requests.values())[-1]
    read, written = newest.get("cache_read_input_tokens", 0), newest.get("cache_creation_input_tokens", 0)
    prompt = newest.get("input_tokens", 0) + read + written
    completion = sum(usage.get("output_tokens", 0) for usage in requests.values())
    return {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion,
            "prompt_tokens_details": {"cached_tokens": read, "cache_write_tokens": written}}


def claude_usage(output, folder=None):
    """Chat-completions usage for the `claude -p` run that answered the call; cache reads are reported apart.
    Read exactly from the session log when it is there (`claude_session_usage`). Otherwise the prompt is
    estimated as one request's: the CLI sums every request of a run (a tool call and the reply after it
    are two), so its total is divided by the run's request count. Checkpoint runs are not the call's
    request and are left out."""
    if folder and output.get("session_id") and "_started" in output:
        exact = claude_session_usage(folder, output["session_id"], output["_started"])
        if exact:
            return exact
    u = output.get("usage") or {}
    requests = max(1, int(output.get("num_turns") or 1))
    prompt = sum(u.get(k, 0) for k in ("input_tokens", "cache_read_input_tokens",
                                       "cache_creation_input_tokens")) // requests
    completion = u.get("output_tokens", 0)
    return {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion,
            "prompt_tokens_details": {"cached_tokens": u.get("cache_read_input_tokens", 0) // requests,
                                      "cache_write_tokens": u.get("cache_creation_input_tokens", 0) // requests}}


def stub_calls(stub):
    return [json.loads(line) for line in (stub / "calls.jsonl").read_text().splitlines()] \
        if (stub / "calls.jsonl").exists() else []


def answer_claude(call, folder, state, delta):
    body = call["body"]
    fresh = not resumable(state, "claude_session", delta)
    session = str(uuid.uuid4()) if fresh else state["claude_session"]
    # WebAgent's tools come from the MCP stub, which records each call for the reply (as for OpenCode).
    (folder / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
    (folder / "calls.jsonl").unlink(missing_ok=True)
    command = claude_command(body, folder)
    prompt = prompt_for(call, delta, fresh, listed=False)
    plan = checkpoint_plan(state, "claude_checkpoint", delta, body) \
        if fresh and "claude_bridge" in fork_support() else None
    started = time.time()
    if plan:
        try:
            base = plan["base"]
            if plan["extend"]:
                start = ["--resume", base["session"], "--fork-session"] if base else ["--session-id", str(uuid.uuid4())]
                made = run_claude(command + start, checkpoint_prompt(call, plan, folder), folder)
                base = state["claude_checkpoint"] = checkpoint_record(plan, state, delta, body,
                                                                      made.get("session_id"))
                (folder / "calls.jsonl").unlink(missing_ok=True)
            if not base["session"]:
                raise RuntimeError("claude reported no checkpoint session")
            command += ["--resume", base["session"], "--fork-session"]
            prompt = continued_prompt(call, len(base["messages"]), folder)
            session = None
        except RuntimeError as error:
            # A checkpoint is an optimization: without one the turn starts fresh as before.
            print(f"{call['id']}: checkpoint skipped ({str(error)[:200]})", file=sys.stderr)
            state.pop("claude_checkpoint", None)
            (folder / "calls.jsonl").unlink(missing_ok=True)
            plan = None
    if not plan:
        command += ["--session-id", session] if fresh else ["--resume", session]
    try:
        output = run_claude(command, prompt, folder)
    except RuntimeError as error:
        if not plan:
            raise
        # The checkpoint could not be forked (for example its session is gone): start fresh once.
        print(f"{call['id']}: checkpoint fork failed ({str(error)[:200]}); starting fresh", file=sys.stderr)
        state.pop("claude_checkpoint", None)
        (folder / "calls.jsonl").unlink(missing_ok=True)
        session = str(uuid.uuid4())
        output = run_claude(claude_command(body, folder) + ["--session-id", session],
                            prompt_for(call, delta, True, listed=False), folder)
    ended = time.time()
    message = parse_reply(json.dumps({"content": output.get("result") or "", "tool_calls": stub_calls(folder)}), body)
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": claude_usage(output, folder)})
    record(call, state, delta, started, ended, output)
    state["claude_session"] = output.get("session_id") or session
    save_state(folder, state)
    prune_claude_sessions(folder, state)
    print(f"{call['id']}: answered chat {call.get('session')} ({'fresh' if fresh else 'resumed'} session {state['claude_session']})")


# The CLI keeps every session it writes, so each fork and checkpoint leaves one behind.
CLI_SESSION_KEEP_SECS = 24 * 3600


def delete_opencode_session(session, work, server=None):
    """Delete one OpenCode session (with its child sessions) WebAgent no longer needs; best effort.
    Without a running server for the chat, a private one opens the same store."""
    if not session:
        return False
    command = [OPENCODE, "session", "delete", session] + (["--server", server] if server else ["--standalone"])
    try:
        done = subprocess.run(command, cwd=work if Path(work).is_dir() else None, capture_output=True, text=True,
                              timeout=60, env={**os.environ, "OPENCODE_SERVER_PASSWORD": server_password()})
    except (OSError, subprocess.TimeoutExpired):
        return False
    if done.returncode:
        print(f"could not delete OpenCode session {session}: {(done.stderr or done.stdout)[:160]}", file=sys.stderr)
    return not done.returncode


def delete_claude_session(session, folder):
    """Delete one Claude session file (and its large-result folder) of the chat folder."""
    project = claude_project(folder)
    (project / f"{session}.jsonl").unlink(missing_ok=True)
    shutil.rmtree(project / session, ignore_errors=True)


def claude_project(folder):
    """Where `claude` stores the sessions of runs started in `folder` (it names the folder after the path)."""
    return Path.home() / ".claude" / "projects" / re.sub(r"[^A-Za-z0-9]", "-", str(folder))


def prune_claude_sessions(folder, state, project=None, now=None):
    """Delete this chat's Claude sessions older than CLI_SESSION_KEEP_SECS: spent forks, superseded
    checkpoints and sessions dropped after a rewrite. The live session and current checkpoint always stay."""
    keep = {state.get("claude_session"), (state.get("claude_checkpoint") or {}).get("session")}
    project = project or claude_project(folder)
    cutoff = (now or time.time()) - CLI_SESSION_KEEP_SECS
    for path in project.glob("*.jsonl"):
        try:
            if path.stem in keep or path.stat().st_mtime > cutoff:
                continue
            path.unlink()
        except OSError:
            continue
        # A session's large tool results sit beside it in a folder of the same name.
        shutil.rmtree(project / path.stem, ignore_errors=True)


def fork_claude(call):
    """Answer a fork call from a fork of the chat's Claude session (`--resume S --fork-session`). It runs in
    the chat folder, where the session lives, with its own stub folder: the chat's session, state and
    tool files are left as they are."""
    folder, state, delta = fork_delta(call)
    body = call["body"]
    session = state.get("claude_session")
    delta = fork_plan(state, delta, body, session)
    stub = folder / "forks" / call["id"]
    stub.mkdir(parents=True, exist_ok=True)
    try:
        (stub / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
        output = run_claude(claude_command(body, stub) + ["--resume", session, "--fork-session"],
                            prompt_for(call, delta, False, listed=False), folder)
        calls = stub_calls(stub)
    finally:
        shutil.rmtree(stub, ignore_errors=True)
    message = parse_reply(json.dumps({"content": output.get("result") or "", "tool_calls": calls}), body)
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": claude_usage(output, folder)})
    print(f"{call['id']}: answered a fork of chat {call.get('session')} (session {session} -> {output.get('session_id')})")
    # WebAgent accepted the summary; the fork is spent.
    forked = output.get("session_id")
    if forked and forked != session:
        delete_claude_session(forked, folder)


# Muse Bridge harness (supplied by the Muse side). There is no Muse CLI: the harness is a live
# Muse agent on this computer. answer_muse writes a handoff file into the chat's muse-inbox/,
# a Muse-side watcher (or a live Muse chat) answers it by writing the reply JSON, and
# answer_muse validates and posts that reply. The handoff names the saved full request, so a
# fresh agent turn can read the whole conversation; state["muse_session"] is a stable per-chat id.
MUSE_PROMPT = ("You are the language model behind a WebAgent agent. Act exactly as that model: follow the "
    "agent instructions and conversation in the prompt and decide the next assistant turn. You cannot run "
    "WebAgent's tools yourself; to use one of the listed WebAgent tools, return it in tool_calls with JSON "
    "arguments matching its schema, and WebAgent runs it and sends you the result. If your prompt shows only "
    "what is new since the last turn and you need earlier context, read the full saved request named in the "
    "handoff before answering. Images appear as [image: /absolute/path] lines; you may view those files. "
    "Write only the reply JSON to the reply path named in the handoff: "
    "{\"content\": str, \"tool_calls\": [{\"name\": str, \"arguments\": {...}}]}.")
MUSE_WAIT_SECS = 25 * 60  # WebAgent fails an acknowledged call at 30 minutes; stop waiting at 25.


def answer_muse(call, folder, state, delta):
    body = call["body"]
    fresh = not state.get("muse_session") or delta["rewritten"] or delta["instructions"] is not None or delta["tools_changed"]
    prompt = prompt_for(call, delta, fresh)
    inbox = folder / "muse-inbox"
    inbox.mkdir(exist_ok=True)
    reply_path = inbox / f"{call['id']}.reply.json"
    reply_path.unlink(missing_ok=True)
    saved = sorted((folder / "requests").glob(f"*-{call['id']}.json"))
    handoff = {"call": call["id"], "session": call.get("session"), "model": body.get("model"),
               "reasoning_effort": body.get("reasoning_effort"), "fresh": fresh,
               "framing": MUSE_PROMPT, "prompt": prompt,
               "request_file": str(saved[-1]) if saved else None,
               "reply_file": str(reply_path), "deadline": time.time() + MUSE_WAIT_SECS}
    (inbox / f"{call['id']}.request.json").write_text(json.dumps(handoff, indent=1))
    started = time.time()
    raw = None
    while time.time() - started < MUSE_WAIT_SECS:
        if reply_path.exists():
            raw = reply_path.read_text()
            try:
                json.loads(raw)
                break
            except ValueError:
                raw = None  # still being written; read it again on the next poll
        time.sleep(2)
    ended = time.time()
    if raw is None:
        raise RuntimeError(f"muse harness did not answer within {MUSE_WAIT_SECS // 60} minutes")
    try:
        message = parse_reply(raw, body)
    except (SystemExit, ValueError, KeyError, TypeError):
        raise RuntimeError("muse reply failed validation against the call's tools")
    # The Muse runtime does not expose per-turn token counts to the answering agent, so
    # usage is None (the reply endpoint accepts that) rather than invented numbers.
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": None})
    record(call, state, delta, started, ended)
    state["muse_session"] = state.get("muse_session") or f"muse-{call.get('session') or 'unknown'}"
    save_state(folder, state)
    print(f"{call['id']}: answered chat {call.get('session')} with {body.get('model')} "
          f"({'fresh' if fresh else 'resumed'} session {state['muse_session']})")


# Bridge provider -> harness that answers its calls. Each takes (call, folder, state, delta).
HARNESSES = {"claude_bridge": answer_claude, "opencode_bridge": answer_opencode, "muse_bridge": answer_muse}
# Bridge provider -> fork harness. Each takes (call); Muse cannot fork.
FORKERS = {"claude_bridge": fork_claude, "opencode_bridge": fork_opencode}


def cmd_auto(session):
    reattach_servers()
    threading.Thread(target=reap_servers, daemon=True).start()
    threading.Thread(target=cleanup_scheduler, daemon=True).start()
    forks = fork_support()
    # One thread per call, no cap: each call already holds its own harness process.
    while True:
        threading.Thread(target=answer, args=(fetch(session, forks),), daemon=True).start()


def selftest():
    assert 25 < POLL_TIMEOUT + 2 < 60
    folder = Path(tempfile.mkdtemp(prefix="harness-bridge-selftest-"))  # fresh: earlier runs leave state
    global SESSIONS
    SESSIONS = folder
    body = {"model": "m", "messages": [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "file_list", "parameters": {}}}]}
    _, state, delta = remember({"id": "a", "session": "s", "body": body})
    save_state(SESSIONS / "s", state)
    assert delta["new"] == body["messages"][1:] and delta["instructions"] and delta["tools_added"] == ["file_list"]
    body["messages"] += [{"role": "assistant", "content": "hello"}, {"role": "user", "content": "again"}]
    _, state, delta = remember({"id": "b", "session": "s", "body": body})
    save_state(SESSIONS / "s", state)
    assert len(delta["new"]) == 2 and delta["instructions"] is None and not delta["rewritten"]
    body["messages"][0]["content"] = "be brief\nuse tools"
    body["tools"].append({"type": "function", "function": {"name": "file_read", "parameters": {}}})
    _, state, delta = remember({"id": "b2", "session": "s", "body": body})
    save_state(SESSIONS / "s", state)
    assert resumable({"opencode_session": "x"}, "opencode_session", delta)
    update = "\n".join(updates(delta, SESSIONS / "s"))
    assert "+use tools" in update and '"file_read"' in update and '"file_list"' not in update
    # A fork reads the chat's state but never writes it, and gets only what is new.
    saved = (SESSIONS / "s" / "state.json").read_text()
    fork_body = json.loads(json.dumps(body))
    fork_body["messages"].append({"role": "user", "content": "summarize"})
    _, _, delta = fork_delta({"id": "f", "session": "s", "body": fork_body})
    assert delta["new"] == [{"role": "user", "content": "summarize"}] and not delta["rewritten"]
    assert (SESSIONS / "s" / "state.json").read_text() == saved
    # A closing call's private closer stays out of the recorded conversation, so the next turn and a
    # fork still append to the session instead of rewriting it.
    closing = {**body, "messages": body["messages"] + [{"role": "user", "content": "label this run"}],
               "webagent_closing": True}
    _, state, delta = remember({"id": "k", "session": "s", "body": closing})
    save_state(SESSIONS / "s", state)
    assert delta["new"] == [{"role": "user", "content": "label this run"}] and not delta["rewritten"]
    later = {**body, "messages": body["messages"] + [{"role": "user", "content": "next task"}]}
    _, _, delta = fork_delta({"id": "f2", "session": "s", "body": later})
    assert delta["new"] == [{"role": "user", "content": "next task"}] and not delta["rewritten"]
    body["messages"] = body["messages"][:1] + [{"role": "user", "content": "summary"}]
    _, _, delta = remember({"id": "c", "session": "s", "body": body})
    assert delta["rewritten"] and len(delta["new"]) == 1
    # After a fold the summary blocks it shares with the previous call go into a checkpoint session;
    # the next fold extends that checkpoint, a short addition reuses it, a new model starts over.
    system, tail = {"role": "system", "content": "i"}, {"role": "user", "content": "tail"}
    # A call-scoped note after the history is shown at the end and never counts as history.
    noted = {}
    note = lambda text: {"role": "system", "content": text}
    changes(folder, noted, {"model": "m", "messages": [system, tail, note("step one")]}, 1)
    nxt = changes(folder, noted, {"model": "m", "messages": [system, tail, {"role": "user", "content": "b"},
                                                             note("step two")]}, 2)
    assert not nxt["rewritten"] and [m["content"] for m in nxt["new"]] == ["b"]
    shown = prompt_for({"body": {"model": "m", "messages": []}}, nxt, False)
    assert shown.endswith("step two") and "step one" not in shown
    # The same guidance on a later step of the same turn is not given to the harness session again.
    again = changes(folder, noted, {"model": "m", "messages": [system, tail, {"role": "user", "content": "b"},
                                                               {"role": "assistant", "content": "c"}, note("step two")]}, 3)
    shown = prompt_for({"body": {"model": "m", "messages": []}}, again, False)
    assert again["notes_repeated"] and "step two" not in shown
    # A closing call carries no guidance and does not count as a change.
    changes(folder, noted, {"model": "m", "webagent_closing": True, "messages": [system, tail,
            {"role": "user", "content": "b"}, {"role": "assistant", "content": "c"}, {"role": "user", "content": "label"}]}, 4)
    # Unchanged guidance goes in again with the next user message.
    later = changes(folder, noted, {"model": "m", "messages": [system, tail, {"role": "user", "content": "b"},
                                   {"role": "assistant", "content": "c"}, {"role": "user", "content": "d"}, note("step two")]}, 5)
    shown = prompt_for({"body": {"model": "m", "messages": []}}, later, False)
    assert not later["notes_repeated"] and shown.endswith("step two")
    # Changed guidance goes in at the next step.
    changed = changes(folder, noted, {"model": "m", "messages": [system, tail, {"role": "user", "content": "b"},
                                     {"role": "assistant", "content": "c"}, {"role": "user", "content": "d"},
                                     {"role": "assistant", "content": "e"}, note("step three")]}, 6)
    shown = prompt_for({"body": {"model": "m", "messages": []}}, changed, False)
    assert shown.endswith("step three")
    # The clean-up schedule: a daily slot fires once after it passes, "every Nh" by elapsed time.
    noon = time.mktime(time.localtime()[:3] + (12, 0, 0, 0, 0, -1))
    assert cleanup_due(["daily 03:00"], noon - 12 * 3600, noon) and not cleanup_due(["daily 03:00"], noon - 3600, noon)
    assert cleanup_due(["daily 03:00, 11:30"], noon - 3600, noon)
    assert cleanup_due(["every 6h"], noon - 7 * 3600, noon) and not cleanup_due(["every 6h"], noon - 3600, noon)
    weekday = DAYS[time.localtime(noon).tm_wday]
    assert cleanup_due([f"weekly {weekday} 11:00"], noon - 3600 * 2, noon)
    assert not cleanup_due([f"weekly {DAYS[(DAYS.index(weekday) + 1) % 7]} 11:00"], noon - 3600 * 2, noon)
    # A fork planned before the chat's later steps still forks, and names where its summary ends.
    raced = {}
    history = [system, tail, {"role": "user", "content": "b"}]
    changes(folder, raced, {"model": "m", "messages": history + [{"role": "user", "content": "later"}]}, 1)
    fork = changes(folder, dict(raced), {"model": "m", "messages": history + [{"role": "user", "content": "summarize"}]}, 0)
    planned = fork_plan(raced, fork, {"model": "m", "messages": history + [{"role": "user", "content": "summarize"}]}, "s")
    assert not planned["rewritten"] and planned["new"][-1]["content"] == "summarize"
    assert "1 more item" in planned["new"][0]["content"] and "\"b\"" in planned["new"][0]["content"]
    try:
        fork_plan(raced, fork, {"model": "m", "messages": [system, {"role": "user", "content": "other"},
                                                          {"role": "user", "content": "summarize"}]}, None)
        raise AssertionError("a fork without a session must be refused")
    except RuntimeError as error:
        assert "no harness session" in str(error)
    blocks = [{"role": "user", "content": f"Summary block {n} " + "x" * CHECKPOINT_MIN_CHARS} for n in (1, 2)]
    chat = {}
    changes(folder, chat, {"model": "m", "messages": [system, blocks[0], {"role": "user", "content": "a"}]}, 1)
    second = {"model": "m", "messages": [system, blocks[0], blocks[1], tail]}
    folded = changes(folder, chat, second, 2)
    plan = checkpoint_plan(chat, "claude_checkpoint", folded, second)
    assert plan == {"base": None, "keep": 1, "extend": True}
    assert "Summary block 1" in checkpoint_prompt({"body": second}, plan, folder) and "Summary block 2" not in \
        checkpoint_prompt({"body": second}, plan, folder)
    assert "Summary block 2" in continued_prompt({"body": second}, 1, folder)
    chat["claude_checkpoint"] = checkpoint_record(plan, chat, folded, second, "cp1")
    third = {"model": "m", "messages": [system, blocks[0], blocks[1], {"role": "user", "content": "block 3"}, tail]}
    later = changes(folder, dict(chat), third, 3)
    assert checkpoint_plan(chat, "claude_checkpoint", later, third) == \
        {"base": chat["claude_checkpoint"], "keep": 2, "extend": True}
    short = {"model": "m", "messages": [system, blocks[0], {"role": "user", "content": "b2"}, tail]}
    assert checkpoint_plan(chat, "claude_checkpoint", changes(folder, dict(chat), short, 3), short) == \
        {"base": chat["claude_checkpoint"], "keep": 1, "extend": False}
    other = dict(third, model="n")
    assert checkpoint_plan(chat, "claude_checkpoint", changes(folder, dict(chat), other, 3), other)["base"] is None
    assert checkpoint_plan(chat, "claude_checkpoint", dict(later, rewritten=False), third) is None
    assert claude_usage({"usage": {"input_tokens": 1, "cache_read_input_tokens": 10, "output_tokens": 2}}) == \
        {"prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13, "prompt_tokens_details": {"cached_tokens": 10, "cache_write_tokens": 0}}
    assert opencode_usage({"input": 0, "output": 0, "cached": 0}) is None
    # Usage read back from the server: only this run's replies, newest one's prompt, outputs summed.
    listed = [{"time": {"created": 300}, "tokens": {"input": 10, "output": 4, "cache": {"read": 90, "write": 2}}},
              {"time": {"created": 250}, "type": "user"},
              {"time": {"created": 200}, "tokens": {"input": 12, "output": 5, "cache": {"read": 80}}},
              {"time": {"created": 100}, "tokens": {"input": 99, "output": 99}}]
    assert steps_usage(listed, 150) == {"input": 102, "output": 9, "cached": 90, "cache_write": 2}
    assert steps_usage(listed, 400) is None
    # Old fork and checkpoint sessions go; the live session, the checkpoint and recent ones stay.
    project = folder / "claude-project"
    project.mkdir()
    for name, age in (("live", 9e5), ("point", 9e5), ("spent", 9e5), ("recent", 60)):
        (project / f"{name}.jsonl").write_text("{}")
        os.utime(project / f"{name}.jsonl", (time.time() - age, time.time() - age))
    (project / "spent").mkdir()
    prune_claude_sessions(folder, {"claude_session": "live", "claude_checkpoint": {"session": "point"}}, project)
    assert sorted(p.name for p in project.iterdir()) == ["live.jsonl", "point.jsonl", "recent.jsonl"]
    assert claude_project(Path("/home/a/x.y/s-1")).name == "-home-a-x-y-s-1"
    # The session log gives exact figures: the newest counted request's prompt, outputs summed.
    log = project / "log-session.jsonl"
    stamp = lambda offset: time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() + offset))
    entry = lambda at, mid, usage: json.dumps({"type": "assistant", "timestamp": stamp(at),
                                               "message": {"id": mid, "usage": usage}})
    log.write_text("\n".join([
        entry(-600, "old", {"input_tokens": 9, "output_tokens": 9}),
        entry(-5, "a", {"input_tokens": 3, "cache_read_input_tokens": 100, "output_tokens": 7}),
        entry(-5, "a", {"input_tokens": 3, "cache_read_input_tokens": 100, "output_tokens": 7}),
        entry(-4, "z", {"input_tokens": 0, "output_tokens": 0}),
        entry(-3, "b", {"input_tokens": 2, "cache_read_input_tokens": 107, "cache_creation_input_tokens": 4,
                        "output_tokens": 5})]))
    assert claude_session_usage(folder, "log-session", time.time() - 60, project) == {
        "prompt_tokens": 113, "completion_tokens": 12, "total_tokens": 125,
        "prompt_tokens_details": {"cached_tokens": 107, "cache_write_tokens": 4}}
    assert claude_session_usage(folder, "missing", 0, project) is None
    # A run that called a tool made two requests: the prompt is one request's, not their sum.
    assert claude_usage({"num_turns": 2, "usage": {"input_tokens": 2, "cache_read_input_tokens": 198_000,
                                                   "output_tokens": 30}})["prompt_tokens"] == 99_001
    message = parse_reply('{"content":"","tool_calls":[{"name":"file_list","arguments":{}}]}', body)
    assert message["tool_calls"][0]["function"] == {"name": "file_list", "arguments": "{}"}
    assert classify("Upstream request failed: [prefill_cap_refused] the cell is at capacity; retry shortly (after 30 s)") \
        == ("overloaded", 30)
    assert classify("['Upstream request failed: Insufficient account funds']")[0] == "quota"
    assert classify("Free usage limit reached. Free models work in OpenCode")[0] == "quota"
    assert classify("ENOENT: no such file") is None
    failure = harness_failure(["Upstream request failed: [overloaded] the server is overloaded; retry shortly"], 1, "")
    assert (failure.source, failure.category, failure.retry_after) == ("upstream", "overloaded", None)
    assert harness_failure([], 1, "crash").source == "harness"
    assert classify({"type": "provider.internal", "status": 503,
                     "message": "[overloaded] the server is overloaded; retry shortly (after 20 s)"}) == ("overloaded", 20)
    assert classify({"type": "provider.internal", "status": 500, "message": "boom"}) == ("upstream", None)
    CURRENT.call = "stop-me"
    process = start_harness(["sleep", "30"])
    stop_calls(["stop-me"])
    assert process.wait(timeout=5) != 0
    try:
        check_stopped()
        raise AssertionError("a stopped call must not reply")
    except Stopped:
        pass
    RUNNING.clear(), STOPPED.clear()
    CURRENT.call = None
    global TIMING
    TIMING = folder / "timing.jsonl"
    fetched = time.time()
    state = {"replied_at": fetched - 3}
    record({"id": "d", "session": "s", "body": body, "fetched_at": fetched}, state, {"new": [{"role": "tool"}]},
           fetched, fetched + 1, {"duration_ms": 800})
    row = json.loads(TIMING.read_text())
    assert not row["user_turn"] and 2.9 < row["gap_s"] < 3.5 and state["replied_at"] >= fetched
    cmd_stats(1)
    # Muse: a stand-in agent answers the handoff (a partial write first); the reply is posted.
    global post
    posted, real_post = [], post
    post = lambda path, payload, timeout=60, url=None: posted.append((path, payload))
    muse_call = {"id": "m", "session": "s", "provider": "muse_bridge", "body": body, "fetched_at": time.time()}
    reply = folder / "s" / "muse-inbox" / "m.reply.json"

    def agent():
        while not (folder / "s" / "muse-inbox" / "m.request.json").exists():
            time.sleep(0.1)
        reply.write_text('{"content": "hi", "tool_')
        time.sleep(2.5)
        reply.write_text('{"content": "hi", "tool_calls": [{"name": "file_list", "arguments": {}}]}')
    threading.Thread(target=agent, daemon=True).start()
    try:
        answer(muse_call)
        # A harness failure is reported at once instead of leaving the call to time out.
        answer({"id": "x", "session": "s", "provider": "unknown_bridge", "body": body})
        # A fork this client cannot make fails at once, so WebAgent summarizes with its System model.
        global FORK_SUPPORT
        FORK_SUPPORT = []
        answer({"id": "y", "session": "s", "provider": "claude_bridge", "fork": True, "body": body})
    finally:
        post = real_post
    # Each call heartbeats as it starts; the replies follow.
    assert any(path.endswith("/progress") and body == {"id": "x"} for path, body in posted)
    posted = [entry for entry in posted if not entry[0].endswith("/progress")]
    assert posted and posted[0][0] == "/api/v3/harness-bridge/reply"
    assert posted[0][1]["message"]["tool_calls"][0]["function"]["name"] == "file_list"
    assert posted[1][1] == {"id": "x", "error": "no harness for unknown_bridge on this client", "source": "harness"}
    assert posted[2][1] == {"id": "y", "error": "no session fork for claude_bridge on this client", "source": "harness"}
    apply_settings({"heartbeat_seconds": 7, "stall_seconds": "x", "probe_seconds": 0})
    assert (HEARTBEAT_SECS, STALL_SECS, PROBE_SECS) == (7, 120, 30)
    print("selftest ok")


# The daily clean-up (`cleanup.json` beside the chat folders, read at each check). A chat idle longer
# than max_idle_hours loses its harness sessions and its folder: WebAgent's history is the record, so
# its next call starts a fresh harness session (an idle session would get no cache hit anyway).
# Active chats keep only their newest request copies (a debugging record).
CLEANUP_FILE = ROOT / "cleanup.json"
CLEANUP_LOG = ROOT / "cleanup.jsonl"
CLEANUP_DEFAULTS = {"enabled": True, "max_idle_hours": 24, "keep_requests_per_chat": 2,
                    "schedule": ["daily 03:00"]}
DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def cleanup_settings():
    try:
        saved = json.loads(CLEANUP_FILE.read_text())
    except (OSError, ValueError):
        saved = {}
    return {**CLEANUP_DEFAULTS, **(saved if isinstance(saved, dict) else {})}


def cleanup_due(schedule, last, now):
    """Whether a slot of `schedule` fell after `last` and by `now` (local time). Slots: "daily HH:MM"
    (several times allowed: "daily 03:00, 15:00"), "weekly mon HH:MM", "every Nh"."""
    for entry in schedule:
        words = entry.replace(",", " ").split()
        if not words:
            continue
        if words[0] == "every" and len(words) == 2 and words[1].endswith("h"):
            if now - last >= float(words[1][:-1]) * 3600:
                return True
            continue
        days = DAYS if words[0] == "daily" else [w for w in words[1:] if w in DAYS] if words[0] == "weekly" else []
        times = [w for w in words[1:] if ":" in w]
        for back in range(8):  # each local day from today back a week
            day = time.localtime(now - back * 86400)
            if DAYS[day.tm_wday] not in days:
                continue
            for hhmm in times:
                hour, minute = (int(x) for x in hhmm.split(":"))
                slot = time.mktime((day.tm_year, day.tm_mon, day.tm_mday, hour, minute, 0, 0, 0, -1))
                if last < slot <= now:
                    return True
    return False


def cleanup(dry_run=False):
    """Run the clean-up once; returns (and logs) what it removed and the sizes before and after."""
    settings = cleanup_settings()
    now = time.time()
    before = usage_report()["total_bytes"]
    removed, trimmed, sessions = [], 0, 0
    for chat in sorted(SESSIONS.iterdir()) if SESSIONS.is_dir() else []:
        if not chat.is_dir():
            continue
        with BUSY_LOCK:
            if chat.name in BUSY:
                continue
        newest = max([chat.stat().st_mtime] + [p.stat().st_mtime for p in chat.rglob("*") if p.is_file()])
        if now - newest >= settings["max_idle_hours"] * 3600:
            state = load_state(chat)
            ids = [state.get("opencode_session"), (state.get("opencode_checkpoint") or {}).get("session")]
            removed.append(chat.name)
            if dry_run:
                continue
            for session in filter(None, ids):
                sessions += delete_opencode_session(session, OPENCODE_HOME / chat.name)
            project = claude_project(chat)
            if project.is_dir():
                sessions += len(list(project.glob("*.jsonl")))
                shutil.rmtree(project, ignore_errors=True)
            for path in (chat, OPENCODE_HOME / chat.name, OPENCODE_HOME / (chat.name + ".fork")):
                shutil.rmtree(path, ignore_errors=True)
            continue
        copies = sorted((chat / "requests").glob("*.json")) if (chat / "requests").is_dir() else []
        for old in copies[:-settings["keep_requests_per_chat"]] if settings["keep_requests_per_chat"] else copies:
            trimmed += 1
            if not dry_run:
                old.unlink(missing_ok=True)
    after = before if dry_run else usage_report()["total_bytes"]
    entry = {"at": now, "dry_run": dry_run, "chats_removed": len(removed), "harness_sessions_deleted": sessions,
             "request_copies_trimmed": trimmed, "bytes_before": before, "bytes_after": after}
    if not dry_run:
        with CLEANUP_LOG.open("a") as log:
            log.write(json.dumps(entry) + "\n")
    return entry


def cleanup_scheduler():
    """Check the clean-up schedule once a minute while the client runs; the last run is kept in the log."""
    last = time.time()
    try:
        last = json.loads(CLEANUP_LOG.read_text().splitlines()[-1])["at"]
    except (OSError, ValueError, IndexError, KeyError):
        pass
    while True:
        time.sleep(60)
        settings = cleanup_settings()
        now = time.time()
        if settings["enabled"] and cleanup_due(settings["schedule"], last, now):
            try:
                print(f"cleanup: {json.dumps(cleanup())}")
            except Exception as error:
                print(f"cleanup failed: {error}", file=sys.stderr)
            last = now


def tree_bytes(path):
    """Bytes of the files under path (0 when it does not exist)."""
    if not path.exists():
        return 0
    if path.is_file():
        return path.stat().st_size
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file() and not p.is_symlink())


def usage_report():
    """What this bridge keeps on this computer: its per-chat folders (saved request copies,
    extracted images, state) and the harnesses' own session stores."""
    chats, requests, images, other, idle = 0, 0, 0, 0, []
    for chat in SESSIONS.iterdir() if SESSIONS.is_dir() else []:
        if not chat.is_dir():
            continue
        chats += 1
        newest = chat.stat().st_mtime
        for path in chat.rglob("*"):
            if not path.is_file() or path.is_symlink():
                continue
            size = path.stat().st_size
            newest = max(newest, path.stat().st_mtime)
            if path.parent.name == "requests":
                requests += size
            elif path.name.startswith("image-"):
                images += size
            else:
                other += size
        idle.append((time.time() - newest) / 86400)
    report = {"bridge": str(ROOT), "chats": chats, "request_copies_bytes": requests,
              "image_bytes": images, "other_chat_bytes": other,
              "chats_idle_over_1_day": sum(1 for days in idle if days >= 1),
              "oldest_idle_days": round(max(idle), 1) if idle else 0}
    database = Path.home() / ".local/share/opencode/opencode.db"
    if database.exists():
        report["opencode_database_bytes"] = sum(
            tree_bytes(Path(str(database) + suffix)) for suffix in ("", "-wal", "-shm"))
        try:
            import sqlite3
            db = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
            report["opencode_sessions"] = db.execute("select count(*) from session_v2").fetchone()[0]
            report["opencode_messages"] = db.execute("select count(*) from session_message").fetchone()[0]
        except Exception:
            pass
        report["opencode_work_bytes"] = tree_bytes(OPENCODE_HOME)
    projects = Path.home() / ".claude/projects"
    prefix = str(SESSIONS).replace("/", "-")
    claude = [p for p in projects.iterdir() if p.name.startswith(prefix)] if projects.is_dir() else []
    if claude:
        report["claude_session_folders"] = len(claude)
        report["claude_session_bytes"] = sum(tree_bytes(p) for p in claude)
    report["total_bytes"] = sum(v for k, v in report.items() if k.endswith("_bytes"))
    return report


def cmd_usage(as_json=False):
    """`usage`: what this bridge keeps on this computer (sizes and counts); `--json` for machines."""
    report = usage_report()
    if as_json:
        print(json.dumps(report))
        return
    for key, value in report.items():
        shown = f"{value / 2**20:,.1f} MB" if key.endswith("_bytes") else value
        print(f"{key:28} {shown}")


if __name__ == "__main__":
    args = sys.argv[1:]
    session = None
    if "--session" in args:
        index = args.index("--session")
        session = args[index + 1]
        del args[index:index + 2]
    if args[:1] == ["pair"] and len(args) in (3, 5) and (len(args) == 3 or args[3] == "--name"):
        cmd_pair(args[1], args[2], args[4] if len(args) == 5 else "")
    elif args[:1] == ["next"]:
        cmd_next(session)
    elif args[:1] == ["reply"] and len(args) == 3:
        cmd_reply(args[1], args[2])
    elif args[:1] == ["auto"]:
        cmd_auto(session)
    elif args[:1] == ["mcp"] and len(args) == 2:
        mcp_stub(args[1])
    elif args[:1] == ["stats"] and len(args) <= 2:
        cmd_stats(float(args[1]) if len(args) == 2 else 24)
    elif args[:1] == ["cleanup"] and len(args) <= 2:
        print(json.dumps(cleanup(dry_run="--dry-run" in args)))
    elif args[:1] == ["usage"] and len(args) <= 2:
        cmd_usage("--json" in args)
    elif args[:1] == ["selftest"]:
        selftest()
    else:
        sys.exit(__doc__)
