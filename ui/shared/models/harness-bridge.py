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

Files live in ../work/harness-bridge/ relative to this script's folder (below: work/harness-bridge/).
Client secret and server: work/harness-bridge/client.json (mode 0600), written by `pair`.
Server override: $HARNESS_BRIDGE_URL.
Per-chat files: work/harness-bridge/sessions/<webagent-session>/.
"""
import base64, difflib, hashlib, json, os, re, secrets, shutil, signal, socket, subprocess, sys, tempfile, threading, time, urllib.error, urllib.request, uuid
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
    "your mcp__harness__* tools (mcp__harness__<name> for a tool listed as <name>). Calling one hands it to "
    "WebAgent, which runs it and sends the result in your next message, so after your tool calls end your turn "
    "without waiting or describing them. Images are saved as files you may Read. Your final text is the agent's reply.")
OPENCODE_PROMPT = ("You are the language model behind a WebAgent agent. Act exactly as that model: follow the "
    "agent instructions and conversation you are given and decide the next assistant turn. WebAgent's tools are "
    "your webagent_* tools (webagent_<name> for a tool listed as <name>). Calling one hands it to WebAgent, which "
    "runs it and sends the result in your next message, so after your tool calls end your turn without waiting. "
    "To run a command use webagent_shell, never bash; to read or write files use WebAgent's file tools. "
    "Never use your other tools: every use is rejected, and this computer is not the agent's workspace.")
# The installer puts opencode in ~/.opencode/bin, which a systemd PATH usually lacks.
OPENCODE = shutil.which("opencode") or str(Path.home() / ".opencode" / "bin" / "opencode")
OPENCODE_HOME = Path.home() / ".cache" / "opencode-bridge"
MCP_PREFIX = "webagent_"


def opencode_config(work, model):
    """Free OpenCode models answer only while the harness lists its own tools, so for them they stay listed
    but "ask" makes the headless run reject every use (never pass --auto). Paid models get "deny", which
    hides them, so the model cannot pick bash over webagent_shell. WebAgent's tools come from this script's
    MCP stub, which records each call for the reply instead of running it; WebAgent's own policy and
    approvals are their only gate."""
    own = "ask" if model.startswith("opencode/") else "deny"
    return json.dumps({
        "mcp": {"webagent": {"type": "local", "enabled": True,
                             "command": [sys.executable, "-B", str(Path(__file__).resolve()), "mcp", str(work)]}},
        "agent": {"webagent-bridge": {"mode": "primary", "prompt": OPENCODE_PROMPT,
                                      "permission": {"*": own, "execute": "allow", MCP_PREFIX + "*": "allow"}}}})


# One `opencode serve` per chat folder and permission mode, kept warm between calls. A fresh
# `opencode run --standalone` starts the model before its MCP stub connects, so every call's first
# step failed with "Unknown tool" and was repeated (35-80 s each on a long context).
SERVERS, SERVERS_LOCK = {}, threading.Lock()
SERVER_PASSWORD = secrets.token_hex(24)
SERVER_IDLE_SECS = 15 * 60


def opencode_server(work, model):
    """URL of a running server for this chat folder, started on first use. Its MCP stub lists tools.json
    as it was at start, so a changed tool set restarts the server."""
    own = "ask" if model.startswith("opencode/") else "deny"
    key = (str(work), own)
    tools = hashlib.sha256((work / "tools.json").read_bytes()).hexdigest()
    with SERVERS_LOCK:
        server = SERVERS.get(key)
        if server and server["process"].poll() is None and server["tools"] == tools:
            server["used"] = time.time()
            return server["url"]
        if server:
            stop_server(server["process"])
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        process = subprocess.Popen([OPENCODE, "serve", "--hostname", "127.0.0.1", "--port", str(port)], cwd=work,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   start_new_session=True,
                                   env={**os.environ, "OPENCODE_CONFIG_CONTENT": opencode_config(work, model),
                                        "OPENCODE_SERVER_PASSWORD": SERVER_PASSWORD})
        deadline = time.time() + 30
        while True:
            try:
                socket.create_connection(("127.0.0.1", port), 0.2).close()
                break
            except OSError:
                if process.poll() is not None or time.time() > deadline:
                    stop_server(process)
                    raise RuntimeError("opencode server did not start")
                time.sleep(0.1)
        SERVERS[key] = {"process": process, "url": f"http://127.0.0.1:{port}", "used": time.time(), "tools": tools}
        return SERVERS[key]["url"]


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
                if server["process"].poll() is not None or time.time() - server["used"] > SERVER_IDLE_SECS:
                    stop_server(server["process"])
                    del SERVERS[key]


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


class Stopped(Exception):
    """WebAgent stopped the call; it wants no reply."""


def interrupt(process, grace=5):
    """Ctrl-C the harness, as its own stop key would; kill it if it is still running after the grace period."""
    def signal_group(sig):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
    if process.poll() is None:
        signal_group(signal.SIGINT)
        threading.Timer(grace, lambda: process.poll() is None and signal_group(signal.SIGKILL)).start()


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


def fetch(session=None):
    """Long-poll until a call arrives and this process takes it; ride out gateway restarts and network blips.
    An unacknowledged call returns to WebAgent's queue after a minute, so a lost response or ack only delays it."""
    while True:
        try:
            response = post("/api/v3/harness-bridge/next", {"session": session} if session else {}, timeout=POLL_TIMEOUT)
            stop_calls(response.get("cancelled") or [])
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


def remember(call):
    """Save the full request and work out what is new for this chat."""
    folder = SESSIONS / (call.get("session") or "unknown")
    folder.mkdir(parents=True, exist_ok=True)
    state_path = folder / "state.json"
    state = json.loads(state_path.read_text()) if state_path.exists() else {}
    body = call["body"]
    (folder / "requests").mkdir(exist_ok=True)
    count = len(list((folder / "requests").iterdir())) + 1
    (folder / "requests" / f"{count:04d}-{call['id']}.json").write_text(json.dumps(call, indent=1))
    instructions, conversation = split(body)
    tools = tools_of(body)
    seen = state.get("conversation", [])
    old_schemas = state.get("tool_schemas", {})
    append = len(seen) <= len(conversation) and all(digest(m) == d for m, d in zip(conversation, seen))
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
    }
    state.update(instructions=digest(instructions), tools=sorted(tools), tools_digest=digest(body.get("tools")),
                 instructions_text=[text_of(m.get("content"), folder) for m in instructions],
                 tool_schemas={n: digest(t) for n, t in tools.items()},
                 conversation=[digest(m) for m in conversation])
    return folder, state, delta


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


def prompt_for(call, delta, fresh):
    """The text the headless session sees: everything on a fresh session, otherwise only what is new."""
    folder, body = delta["folder"], call["body"]
    lines = []
    if fresh:
        instructions, conversation = split(body)
        lines.append("## Agent instructions\n" + "\n\n".join(text_of(m.get("content"), folder) for m in instructions))
        lines.append("## Tools you may call\n" + json.dumps([t["function"] for t in tools_of(body).values()], indent=1))
        messages = conversation
    else:
        lines += updates(delta, folder)
        messages = delta["new"]
    lines.append("## Conversation" if fresh else "## New since your last turn")
    lines += [render(m, folder, full=True) for m in messages]
    return "\n\n".join(lines)


def updates(delta, folder):
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
    if delta["tool_updates"]:
        lines.append("## Tools added or changed\n" + json.dumps(delta["tool_updates"], indent=1))
    return lines


def resumable(state, key, delta):
    """Restart only when the transcript was rewritten (compaction or edits); other changes are appended."""
    return bool(state.get(key)) and not delta["rewritten"]


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


def opencode_turn(command, prompt, work, model):
    """Run one `opencode run`; return its session id, the final text and token usage.

    The run stops at the end of the first step that called a WebAgent tool: the stub only records the call,
    so any further step is the model winding down (30-45 s each on a long context). The next call resumes
    the session with the tool's real result."""
    # The prompt goes on stdin: a fresh session's full history can exceed the 128 KiB argument cap.
    process = start_harness(command, cwd=work, env={**os.environ, "OPENCODE_SERVER_PASSWORD": SERVER_PASSWORD})
    stop = lambda: interrupt(process)
    timer = threading.Timer(HARNESS_SECS, stop)
    timer.start()
    stderr = []
    threading.Thread(target=lambda: stderr.append(process.stderr.read()), daemon=True).start()
    process.stdin.write(prompt)
    process.stdin.close()
    session, texts, usage, errors, stopped = None, {}, {"input": 0, "output": 0}, [], False
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
                usage["input"] += tokens.get("input", 0) + cache.get("read", 0) + cache.get("write", 0)
                usage["output"] += tokens.get("output", 0)
                if (work / "calls.jsonl").exists() and (work / "calls.jsonl").stat().st_size:
                    stopped = True
                    stop()
                    break
            elif event.get("type") == "error":
                errors.append(str((event.get("error") or {}).get("message", event))[:300])
        process.wait()
    finally:
        timer.cancel()
        stop()
    check_stopped()
    if errors or not session or (process.returncode and not stopped):
        raise RuntimeError(f"opencode failed ({process.returncode}): {errors or ''.join(stderr)[-300:]}")
    # The last assistant message is the turn's answer; earlier ones led up to its tool calls.
    return session, "".join(list(texts.values())[-1]) if texts else "", usage


def answer_opencode(call, folder, state, delta):
    body = call["body"]
    fresh = not resumable(state, "opencode_session", delta)
    prompt = prompt_for(call, delta, fresh)
    work = OPENCODE_HOME / folder.name
    work.mkdir(parents=True, exist_ok=True)
    (work / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
    command = [OPENCODE, "run", "--server", opencode_server(work, body.get("model", "")), "--agent", "webagent-bridge",
               "-m", body.get("model", ""), "--format", "json"]
    if not fresh:
        command += ["-s", state["opencode_session"]]
    for image in re.findall(r"\[image: (/[^\]]+)\]", prompt):
        command += ["-f", image]
    # The work folder is outside this repository: OpenCode would otherwise load the enclosing git project.
    (work / "calls.jsonl").unlink(missing_ok=True)
    started = time.time()
    session, text, usage = opencode_turn(command, prompt, work, body.get("model", ""))
    ended = time.time()
    calls = [json.loads(line) for line in (work / "calls.jsonl").read_text().splitlines()] \
        if (work / "calls.jsonl").exists() else []
    calls = [{"name": c["name"].removeprefix(MCP_PREFIX), "arguments": c["arguments"]} for c in calls]
    message = parse_reply(json.dumps({"content": text, "tool_calls": calls}), body)
    # Some models report no step usage; send none rather than zeros.
    usage = {"prompt_tokens": usage["input"], "completion_tokens": usage["output"],
             "total_tokens": usage["input"] + usage["output"]} if usage["input"] or usage["output"] else None
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": usage})
    record(call, state, delta, started, ended)
    state["opencode_session"] = session
    save_state(folder, state)
    print(f"{call['id']}: answered chat {call.get('session')} with {body.get('model')} "
          f"({'fresh' if fresh else 'resumed'} session {session}, {len(calls)} tool call(s))")


def check_stopped():
    if getattr(CURRENT, "call", None) in STOPPED:
        raise Stopped()


def answer(call):
    """Run the call's harness; on any failure tell WebAgent at once, so the run retries instead of waiting."""
    CURRENT.call = call["id"]
    try:
        harness = HARNESSES.get(call.get("provider"))
        if not harness:
            raise RuntimeError(f"no harness for {call.get('provider')} on this client")
        folder, state, delta = remember(call)
        harness(call, folder, state, delta)
    except Stopped:
        print(f"{call['id']}: stopped; no reply sent", file=sys.stderr)
    except (Exception, SystemExit) as error:
        detail = str(error)[:300] or type(error).__name__
        print(f"{call['id']}: {detail}", file=sys.stderr)
        try:
            post("/api/v3/harness-bridge/reply", {"id": call["id"], "error": detail})
        except (Transient, Gone) as failure:
            print(f"{call['id']}: could not report the failure ({failure})", file=sys.stderr)
    finally:
        RUNNING.pop(call["id"], None)
        STOPPED.discard(call["id"])


def answer_claude(call, folder, state, delta):
    body = call["body"]
    fresh = not resumable(state, "claude_session", delta)
    session = str(uuid.uuid4()) if fresh else state["claude_session"]
    # WebAgent's tools come from the MCP stub, which records each call for the reply (as for OpenCode).
    (folder / "tools.json").write_text(json.dumps([t["function"] for t in tools_of(body).values()]))
    (folder / "calls.jsonl").unlink(missing_ok=True)
    # The CLI reserves the server name "webagent" and silently skips it.
    mcp = {"mcpServers": {"harness": {"command": sys.executable,
                                      "args": ["-B", str(Path(__file__).resolve()), "mcp", str(folder)]}}}
    command = ["claude", "-p", "--output-format", "json", "--system-prompt", AUTO_PROMPT, "--tools", "Read",
               "--mcp-config", json.dumps(mcp), "--allowedTools", "mcp__harness", "--model", body.get("model", ""),
               *BARE]
    if body.get("reasoning_effort"):
        command += ["--effort", body["reasoning_effort"]]
    command += ["--session-id", session] if fresh else ["--resume", session]
    # Prompt on stdin: a fresh chat's full history exceeds Linux's 128 KiB single-argument limit.
    prompt = prompt_for(call, delta, fresh)
    started = time.time()
    process = start_harness(command, cwd=folder)
    try:
        stdout, stderr = process.communicate(prompt, timeout=HARNESS_SECS)
    except subprocess.TimeoutExpired:
        interrupt(process)
        stdout, stderr = process.communicate()
    check_stopped()
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    ended = time.time()
    try:
        output = json.loads(result.stdout)
    except ValueError:
        raise RuntimeError(f"claude failed ({result.returncode}): {result.stderr[:300]}")
    if output.get("is_error"):
        raise RuntimeError(f"claude error: {str(output.get('result'))[:300]}")
    calls = [json.loads(line) for line in (folder / "calls.jsonl").read_text().splitlines()] \
        if (folder / "calls.jsonl").exists() else []
    message = parse_reply(json.dumps({"content": output.get("result") or "", "tool_calls": calls}), body)
    usage = output.get("usage") or {}
    prompt_tokens = sum(usage.get(k, 0) for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
    completion_tokens = usage.get("output_tokens", 0)
    post("/api/v3/harness-bridge/reply", {"id": call["id"], "message": message, "usage": {
        "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": prompt_tokens + completion_tokens}})
    record(call, state, delta, started, ended, output)
    state["claude_session"] = output.get("session_id") or session
    save_state(folder, state)
    print(f"{call['id']}: answered chat {call.get('session')} ({'fresh' if fresh else 'resumed'} session {state['claude_session']})")


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


def cmd_auto(session):
    threading.Thread(target=reap_servers, daemon=True).start()
    # One thread per call, no cap: each call already holds its own harness process.
    while True:
        threading.Thread(target=answer, args=(fetch(session),), daemon=True).start()


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
    body["messages"] = body["messages"][:1] + [{"role": "user", "content": "summary"}]
    _, _, delta = remember({"id": "c", "session": "s", "body": body})
    assert delta["rewritten"] and len(delta["new"]) == 1
    message = parse_reply('{"content":"","tool_calls":[{"name":"file_list","arguments":{}}]}', body)
    assert message["tool_calls"][0]["function"] == {"name": "file_list", "arguments": "{}"}
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
    finally:
        post = real_post
    assert posted and posted[0][0] == "/api/v3/harness-bridge/reply"
    assert posted[0][1]["message"]["tool_calls"][0]["function"]["name"] == "file_list"
    assert posted[1][1] == {"id": "x", "error": "no harness for unknown_bridge on this client"}
    print("selftest ok")


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
    elif args[:1] == ["selftest"]:
        selftest()
    else:
        sys.exit(__doc__)
