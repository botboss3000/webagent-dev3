"""Per-call agent-scoped HTTPS transport; credentials never become tool output."""
import contextvars
import importlib.util
import ipaddress
import os
from pathlib import Path
import re
import socket
from urllib.parse import urlsplit

_DIRECTORY = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("v3_git_adapter_private", _DIRECTORY / "adapter.py")
adapter = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(adapter)
_CONNECTIONS = contextvars.ContextVar("v3_git_connections", default=())


def normalized(raw):
    if not isinstance(raw, str) or len(raw) > 2048 or raw.strip() != raw or any(ord(c) < 33 for c in raw):
        raise adapter.Rejected("Invalid configured HTTPS repository")
    value = urlsplit(raw)
    try:
        port = value.port
    except ValueError:
        raise adapter.Rejected("Invalid repository port")
    if value.scheme != "https" or not value.hostname or value.username or value.password or value.query or value.fragment or not value.path or value.path == "/" or "%" in value.path:
        raise adapter.Rejected("HTTPS repository without embedded credentials required")
    host = value.hostname.lower()
    if ":" in host:
        host = "[" + host + "]"
    authority = host + ((":" + str(port)) if port is not None and port != 443 else "")
    if any(p in (".", "..") for p in value.path.split("/")):
        raise adapter.Rejected("Normalized repository path required")
    return "https://" + authority + value.path.rstrip("/")


def public_host(url):
    """Refuse repositories on loopback, private, link-local, Tailscale or metadata addresses."""
    # shortcut: git resolves the name again when it dials, so DNS rebinding between the two
    # lookups is not covered; pin the address through a proxy if that becomes a concern.
    host = urlsplit(url).hostname
    try:
        addresses = {info[4][0] for info in socket.getaddrinfo(host, 443, proto=socket.IPPROTO_TCP)}
    except OSError:
        raise adapter.Rejected("Repository host does not resolve")
    for address in addresses:
        ip = ipaddress.ip_address(address.split("%")[0])
        if getattr(ip, "ipv4_mapped", None):
            ip = ip.ipv4_mapped
        if not ip.is_global or ip.is_multicast:
            raise adapter.Rejected("Repository host must be a public address")


def connections(trusted):
    if not isinstance(trusted, dict):
        return ()
    trusted = trusted.get("repository", trusted)
    rows = trusted.get("connections", [])
    if not isinstance(rows, list) or len(rows) > 8:
        raise adapter.Rejected("Invalid trusted repository connections")
    result = []
    for row in rows:
        if not isinstance(row, dict) or row.get("revoked") is True:
            continue
        url = normalized(row.get("url"))
        username, token = row.get("username"), row.get("token")
        if not isinstance(username, str) or not 1 <= len(username) <= 128 or not all(33 <= ord(c) <= 126 for c in username) or any(c in username for c in "'\"\\@:"):
            raise adapter.Rejected("Invalid trusted repository username")
        if token is not None and (not isinstance(token, str) or not 8 <= len(token) <= 512 or not all(33 <= ord(c) <= 126 for c in token)):
            raise adapter.Rejected("Invalid trusted repository credential")
        if any(previous["url"] == url for previous in result):
            raise adapter.Rejected("Ambiguous repository connection")
        result.append({"url": url, "username": username, "token": token})
    return tuple(result)


async def authorize(repo, remote, operation):
    if operation not in ("fetch", "pull", "push"):
        raise adapter.Rejected("Unsupported repository transport operation")
    url = (await adapter.command(repo, ["remote", "get-url", remote])).decode().strip()
    exact = normalized(url)
    row = next((row for row in _CONNECTIONS.get() if row["url"] == exact), None)
    if row is None:
        raise adapter.Rejected("Remote is not granted by this agent's repository connections")
    public_host(exact)
    # Git executes GIT_ASKPASS directly as one filename, without shell parsing.
    # Its fixed shebang supplies isolated Python; the sandbox allows only this
    # trusted helper, never an executable selected by repository configuration.
    helper = _DIRECTORY / "askpass.py"
    if not os.access(helper, os.X_OK):
        raise adapter.Rejected("Trusted Git credential helper is not executable")
    return {"GIT_ASKPASS": str(helper),
            "V3_GIT_URL": exact, "V3_GIT_USERNAME": row["username"],
            "V3_GIT_TOKEN": row["token"] or ""}


adapter.NETWORK_TRANSPORT = authorize


async def execute(name, arguments, base, trusted=None):
    try:
        rows = connections(trusted)
    except adapter.Rejected as error:
        return {"status": "error", "error_kind": error.kind, "output": str(error)}
    lease = _CONNECTIONS.set(rows)
    try:
        return await adapter.execute(name, arguments, base)
    finally:
        _CONNECTIONS.reset(lease)
