"""Egress guard for the signed-out browser: a local proxy that only reaches public addresses.

Chromium sends every request here, loopback included (`--proxy-bypass-list=<-loopback>`).
Each CONNECT or absolute-URI request is resolved here, refused when any resolved address
is not public, and connected to the checked address, so DNS cannot swap in an internal
address after the check. One request per client connection, so a reused proxy connection
cannot reach a second, unchecked host.
"""
import ipaddress
import select
import socket
import socketserver
import threading
import time
from urllib.parse import urlsplit

PORTS = {80, 443}
MAX_HEADER = 16384
IDLE = 60
REFUSALS = 64


def public(address):
    """True only for globally routable unicast addresses (not private, Tailscale, link-local or reserved)."""
    ip = ipaddress.ip_address(address)
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return ip.is_global and not ip.is_multicast


def resolve(host, port, lookup=socket.getaddrinfo):
    """The address to connect to, or a refusal reason when the host is not wholly public."""
    if port not in PORTS:
        return None, "only ports 80 and 443 are allowed"
    try:
        found = lookup(host, port, type=socket.SOCK_STREAM)
    except (OSError, UnicodeError):
        return None, "the name does not resolve"
    addresses = [item[4][0] for item in found]
    if not addresses:
        return None, "the name does not resolve"
    if not all(public(address) for address in addresses):
        return None, "it resolves to a private or internal address"
    return addresses[0], None


class Guard:
    """Threaded proxy bound to loopback; `refusals` keeps the latest reason per host for the browser's error text."""

    def __init__(self, lookup=socket.getaddrinfo):
        self.lookup = lookup
        self.refusals = {}
        self.lock = threading.Lock()
        guard = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                guard.serve(self.request)

        class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
            daemon_threads = True
            allow_reuse_address = True

        self.server = Server(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def refused(self, host, reason):
        with self.lock:
            self.refusals[host] = (reason, time.monotonic())
            while len(self.refusals) > REFUSALS:
                self.refusals.pop(next(iter(self.refusals)))

    def reason(self, host, within=30):
        """The recent refusal reason for `host`, if any."""
        with self.lock:
            found = self.refusals.get(host)
        return found[0] if found and time.monotonic() - found[1] <= within else None

    def serve(self, client):
        client.settimeout(IDLE)
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = client.recv(4096)
            if not chunk or len(head) + len(chunk) > MAX_HEADER:
                return
            head += chunk
        line, _, rest = head.partition(b"\r\n")
        try:
            method, target, version = line.decode("latin-1").split(" ", 2)
        except ValueError:
            return
        if method == "CONNECT":
            host, _, port = target.rpartition(":")
            host = host.strip("[]").lower()
            port = int(port) if port.isdigit() else 0
        else:
            parsed = urlsplit(target)
            if parsed.scheme != "http" or not parsed.hostname:
                client.sendall(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                return
            host, port = parsed.hostname.lower(), parsed.port or 80
        address, reason = resolve(host, port, self.lookup)
        if reason:
            self.refused(host, reason)
            client.sendall(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            return
        try:
            upstream = socket.create_connection((address, port), timeout=IDLE)
        except OSError:
            client.sendall(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            return
        with upstream:
            if method == "CONNECT":
                client.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                early = rest.partition(b"\r\n\r\n")[2]
                if early:
                    upstream.sendall(early)
            else:
                parsed = urlsplit(target)
                path = (parsed.path or "/") + ("?" + parsed.query if parsed.query else "")
                block, _, body = rest.partition(b"\r\n\r\n")
                headers = [h for h in block.split(b"\r\n") if h and not h.lower().startswith((b"proxy-connection:", b"connection:", b"keep-alive:"))]
                # Connection: close keeps one checked host per client connection.
                request = (f"{method} {path} {version}\r\n".encode("latin-1") + b"\r\n".join(headers)
                           + b"\r\nConnection: close\r\n\r\n" + body)
                upstream.sendall(request)
            relay(client, upstream)


def relay(left, right):
    """Copy bytes both ways until either side closes or the connection idles out."""
    sockets = [left, right]
    while True:
        ready, _, _ = select.select(sockets, [], [], IDLE)
        if not ready:
            return
        for source in ready:
            data = source.recv(65536)
            if not data:
                return
            (right if source is left else left).sendall(data)
