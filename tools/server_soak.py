#!/usr/bin/env python3
"""Isolated Linux resource/delivery run using only Python's standard library.

Example: python3 tools/server_soak.py --binary target/release/poknited
Use --duration 86400 for an actual 24-hour acceptance run. This never uses
production credentials or data; it starts a fresh loopback-only server.
"""
import argparse
import base64
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import secrets
import selectors
import socket
import sqlite3
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid


class WebSocket:
    def __init__(self, host, port, token, cursor=0, tls=None):
        self.socket = socket.create_connection((host, port), timeout=5)
        if tls is not None:
            self.socket = tls.wrap_socket(self.socket, server_hostname=host)
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        request = (f"GET /v2/stream?after={cursor} HTTP/1.1\r\nHost: {host}:{port}\r\n"
                   "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
                   f"Sec-WebSocket-Key: {key}\r\nAuthorization: Bearer {token}\r\n\r\n")
        self.socket.sendall(request.encode())
        response = bytearray()
        while b"\r\n\r\n" not in response:
            part = self.socket.recv(8192)
            if not part:
                raise RuntimeError("WebSocket handshake disconnected")
            response.extend(part)
            if len(response) > 65536:
                raise RuntimeError("Oversized WebSocket handshake")
        headers, extra = bytes(response).split(b"\r\n\r\n", 1)
        expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        parsed = {name.strip().lower(): value.strip() for name,value in
                  (line.split(b":",1) for line in headers.split(b"\r\n")[1:] if b":" in line)}
        if not headers.startswith(b"HTTP/1.1 101 ") or parsed.get(b"sec-websocket-accept") != expected:
            # No request headers (including credentials) are included in the failure.
            raise RuntimeError("WebSocket upgrade was rejected")
        self.buffer = bytearray(extra)
        self.fragments = bytearray()

    def send(self, opcode, body):
        mask = secrets.token_bytes(4)
        n = len(body)
        if n < 126:
            header = bytes([0x80 | opcode, 0x80 | n])
        else:
            header = bytes([0x80 | opcode, 0xFE]) + struct.pack("!H", n)
        self.socket.sendall(header + mask + bytes(x ^ mask[i % 4] for i, x in enumerate(body)))

    def ack(self, cursor):
        self.send(1, json.dumps({"type": "ack", "cursor": cursor}, separators=(",", ":")).encode())

    def events(self, data=b""):
        self.buffer.extend(data)
        events = []
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            if second & 0x80:
                raise RuntimeError("Masked server frame")
            length = second & 0x7F
            offset = 2
            if length == 126:
                if len(self.buffer) < 4:
                    break
                length = struct.unpack("!H", self.buffer[2:4])[0]
                offset = 4
            elif length == 127:
                if len(self.buffer) < 10:
                    break
                length = struct.unpack("!Q", self.buffer[2:10])[0]
                offset = 10
            if length > 32768:
                raise RuntimeError("Oversized server frame")
            if len(self.buffer) < offset + length:
                break
            body = bytes(self.buffer[offset:offset + length])
            del self.buffer[:offset + length]
            opcode = first & 0x0F
            if opcode == 9:
                self.send(10, body)
            elif opcode == 8:
                raise EOFError("WebSocket closed by server")
            elif opcode in (0, 1):
                self.fragments.extend(body)
                if len(self.fragments) > 32768:
                    raise RuntimeError("Oversized server message")
                if first & 0x80:
                    events.append(json.loads(self.fragments))
                    self.fragments.clear()
            elif opcode != 10:
                raise RuntimeError("Unexpected WebSocket opcode")
        return events

    def close(self):
        self.socket.close()


def json_request(base, path, token=None, body=None, context=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(base + path, headers=headers,
                                     data=None if body is None else json.dumps(body).encode())
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))
    with opener.open(request, timeout=5) as response:
        return json.load(response)


def wait_ready(base, process, context=None):
    last_error = None
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError("Server exited before readiness")
        try:
            json_request(base, "/healthz", context=context)
            return
        except (OSError, urllib.error.URLError) as error:
            last_error = error
            time.sleep(0.05)
    raise RuntimeError("Server did not become ready") from last_error


def process_stats(pid):
    status = Path(f"/proc/{pid}/status").read_text()
    values = {}
    for line in status.splitlines():
        if line.startswith(("VmRSS:", "VmHWM:")):
            name, value, _ = line.split()
            values[name.rstrip(":")] = int(value) * 1024
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    values["cpu_seconds"] = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    return values


def run(args):
    binary = Path(args.binary).resolve()
    with tempfile.TemporaryDirectory(prefix="poknite-soak-") as temporary:
        root = Path(temporary)
        reserved = socket.socket()
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
        reserved.close()
        config = root / "poknite.toml"
        data = root / "data"
        config.write_text(f'listen = "127.0.0.1:{port}"\ndata_dir = "{data}"\ne2ee_required=false\nretention_seconds = {args.retention}\n')
        subprocess.run([str(binary), "--config", str(config), "init"], check=True, stdout=subprocess.DEVNULL)
        tokens = [secrets.token_hex(32) for _ in range(args.devices)]
        with sqlite3.connect(data / "server.db") as connection:
            for user in range(1, math.ceil(args.devices / 3) + 1):
                connection.execute("INSERT INTO users(id,name) VALUES(?,?)", (user, f"Fixture {user}"))
                connection.execute("INSERT INTO memberships VALUES(?,1)", (user,))
            for i, token in enumerate(tokens):
                connection.execute("INSERT INTO devices(user_id,name,token_hash,created_at) VALUES(?,?,?,?)",
                                   (i // 3 + 1, f"Fixture {i + 1}", hashlib.sha256(token.encode()).hexdigest(), int(time.time())))
        stderr = open(root / "server-stderr.log", "wb")
        process = subprocess.Popen([str(binary), "--config", str(config), "serve", "--dev-http"],
                                   stdout=subprocess.DEVNULL, stderr=stderr)
        sockets = []
        selector = selectors.DefaultSelector()
        try:
            base = f"http://127.0.0.1:{port}"
            wait_ready(base, process)
            for token in tokens:
                ws = WebSocket("127.0.0.1", port, token)
                ws.socket.setblocking(False)
                sockets.append(ws)
                selector.register(ws.socket, selectors.EVENT_READ, ws)
            start = time.monotonic()
            initial_stats = process_stats(process.pid)
            peak_rss = initial_stats["VmRSS"]
            peak_database = peak_wal = 0
            sent = {}
            receipts = {}
            latencies = []
            published = quota_rejections = duplicates = 0
            synchronized = set()
            next_publish = start + 2
            while time.monotonic() - start < args.duration:
                if process.poll() is not None:
                    raise RuntimeError("Server exited during soak")
                if not args.idle and time.monotonic() >= next_publish:
                    now = time.monotonic()
                    text = "resource-probe " + str(published) + " "
                    text += "x" * max(0, args.message_bytes - len(text))
                    try:
                        result = json_request(base, "/v2/conversations/1/messages", tokens[0],
                                              {"client_message_id": str(uuid.uuid4()), "text": text})
                        sent[result["id"]] = now
                        receipts[result["id"]] = set()
                        published += 1
                    except urllib.error.HTTPError as error:
                        if error.code != 507:
                            raise
                        quota_rejections += 1
                    next_publish = now + args.publish_interval
                # Process handshake surplus as well as newly readable bytes.
                for ws in sockets:
                    if ws.buffer:
                        for event in ws.events():
                            if event["type"] == "synced":
                                synchronized.add(id(ws))
                                ws.ack(event["cursor"])
                for key, _ in selector.select(timeout=0.2):
                    ws = key.data
                    chunk = ws.socket.recv(65536)
                    if not chunk:
                        raise EOFError("A client disconnected during soak")
                    for event in ws.events(chunk):
                        kind = event["type"]
                        if kind in ("progress", "synced"):
                            ws.ack(event["cursor"])
                            if kind == "synced":
                                synchronized.add(id(ws))
                        elif kind == "message":
                            identity = event["message"]["id"]
                            if identity not in sent:
                                raise RuntimeError("Received an unknown message")
                            if id(ws) in receipts[identity]:
                                duplicates += 1
                            else:
                                receipts[identity].add(id(ws))
                                latencies.append(time.monotonic() - sent[identity])
                stats = process_stats(process.pid)
                peak_rss = max(peak_rss, stats["VmRSS"])
                peak_database = max(peak_database, (data / "server.db").stat().st_size)
                wal = data / "server.db-wal"
                peak_wal = max(peak_wal, wal.stat().st_size if wal.exists() else 0)
            final_stats = process_stats(process.pid)
            elapsed = time.monotonic() - start
            missing = sum(args.devices - len(value) for value in receipts.values())
            ordered = sorted(latencies)
            report = {"platform": platform.platform(), "glibc": platform.libc_ver(),
                      "duration_seconds": round(elapsed, 2), "devices": args.devices,
                      "mode": "idle" if args.idle else "delivery", "synchronized_devices": len(synchronized),
                      "published": published, "deliveries": len(latencies), "missing": missing,
                      "duplicates": duplicates, "storage_full_responses": quota_rejections,
                      "latency_p95_seconds": round(ordered[max(0, math.ceil(len(ordered) * 0.95) - 1)], 4) if ordered else None,
                      "binary_bytes": binary.stat().st_size, "rss_initial_bytes": initial_stats["VmRSS"],
                      "rss_final_bytes": final_stats["VmRSS"], "rss_peak_bytes": peak_rss,
                      "rss_process_high_water_bytes": final_stats["VmHWM"],
                      "cpu_seconds": round(final_stats["cpu_seconds"] - initial_stats["cpu_seconds"], 3),
                      "database_peak_bytes": peak_database, "wal_peak_bytes": peak_wal,
                      "server_stderr_bytes": (root / "server-stderr.log").stat().st_size,
                      "full_day_verified": elapsed >= 86400,
                      "passed": missing == 0 and duplicates == 0 and len(synchronized) == args.devices
                      and peak_rss <= 32 * 1024 * 1024 and binary.stat().st_size <= 10 * 1024 * 1024
                      and peak_database <= 32 * 1024 * 1024 and peak_wal <= 4 * 1024 * 1024
                      and (not ordered or ordered[max(0, math.ceil(len(ordered) * .95) - 1)] <= 2)}
            encoded = json.dumps(report, ensure_ascii=False, indent=2)
            print(encoded)
            if args.report:
                Path(args.report).write_text(encoded + "\n")
            return 0 if report["passed"] else 1
        finally:
            for ws in sockets:
                ws.close()
            selector.close()
            process.terminate()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            stderr.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/poknited")
    parser.add_argument("--duration", type=float, default=60)
    parser.add_argument("--devices", type=int, default=60)
    parser.add_argument("--retention", type=int, default=86400)
    parser.add_argument("--publish-interval", type=float, default=5)
    parser.add_argument("--message-bytes", type=int, default=64)
    parser.add_argument("--idle", action="store_true")
    parser.add_argument("--report")
    arguments = parser.parse_args()
    if not (1 <= arguments.devices <= 60 and arguments.duration >= 3 and arguments.publish_interval >= 2
            and 1 <= arguments.retention <= 2592000 and 32 <= arguments.message_bytes <= 4096):
        parser.error("Invalid devices/duration/retention/message size; publish interval must be >= 2 seconds")
    raise SystemExit(run(arguments))
