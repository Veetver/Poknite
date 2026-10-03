#!/usr/bin/env python3
"""Real HTTPS/WSS verification, certificate rotation and admin revocation smoke.

Requires Linux, Python standard library and openssl. Uses disposable CA,
keys, credentials and server data. Never disables TLS certificate checking.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import time
import urllib.error
import uuid
from server_soak import WebSocket, json_request, wait_ready


def openssl(*args):
    result = subprocess.run(["openssl", *map(str, args)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        raise RuntimeError("Fixture certificate generation failed: " + result.stderr)


def certificate(root, name, days=2):
    key, request, pem = (root / f"{name}.{suffix}" for suffix in ("key", "csr", "pem"))
    openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", request, "-subj", "/CN=localhost")
    extensions = root / "extensions.txt"
    extensions.write_text("subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n")
    if days < 0:
        # `openssl ca` accepts explicit validity ranges on Ubuntu 24.04 too.
        index, serial = root / "ca.index", root / "ca.serial"
        if not index.exists():
            index.write_text("")
            serial.write_text("1000\n")
        ca_config = root / "ca.conf"
        ca_config.write_text(f"[ca]\ndefault_ca=fixture\n[fixture]\ndatabase={index}\nserial={serial}\nnew_certs_dir={root}\ncertificate={root / 'ca.pem'}\nprivate_key={root / 'ca.key'}\ndefault_md=sha256\npolicy=any_name\nunique_subject=no\n[any_name]\ncommonName=supplied\n[leaf]\n" + extensions.read_text())
        start, end = ("20000101000000Z", "20000102000000Z") if days == -1 else ("20990101000000Z", "20990102000000Z")
        openssl("ca", "-batch", "-notext", "-config", ca_config, "-in", request, "-out", pem,
                "-startdate", start, "-enddate", end, "-extensions", "leaf")
    else:
        openssl("x509", "-req", "-in", request, "-CA", root / "ca.pem", "-CAkey", root / "ca.key",
                "-set_serial", str(secrets.randbits(120)), "-days", str(days), "-extfile", extensions, "-out", pem)
    return pem, key


def peer_fingerprint(port, context, hostname="localhost"):
    with socket.create_connection(("127.0.0.1", port), timeout=3) as raw:
        with context.wrap_socket(raw, server_hostname=hostname) as secured:
            return hashlib.sha256(secured.getpeercert(binary_form=True)).hexdigest()


def wait_logged(logfile, before):
    for _ in range(100):
        lines = logfile.read_text().splitlines()
        if len(lines) > before:
            return lines[-1]
        time.sleep(0.05)
    raise AssertionError("No result of SIGHUP reload")


def receive_until(ws, kind):
    for _ in range(100):
        for event in ws.events():
            if event["type"] == kind:
                return event
        data = ws.socket.recv(65536)
        if not data:
            raise EOFError("TLS WebSocket disconnected")
        # Preserve all parsed frames until next iteration, avoiding loss of surplus.
        ws.buffer.extend(data)
    raise AssertionError("Expected WebSocket event did not arrive")


def run(binary, report_path):
    with tempfile.TemporaryDirectory(prefix="poknite-tls-") as temporary:
        root = Path(temporary)
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "2",
                "-keyout", root / "ca.key", "-out", root / "ca.pem", "-subj", "/CN=Poknite disposable test CA",
                "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign")
        first_cert, first_key = certificate(root, "first")
        second_cert, second_key = certificate(root, "second")
        expired_cert, expired_key = certificate(root, "expired", days=-1)
        future_cert, future_key = certificate(root, "future", days=-2)
        context = ssl.create_default_context(cafile=str(root / "ca.pem"))
        reserved = socket.socket()
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
        reserved.close()
        config = root / "poknite.toml"
        data = root / "data"

        def write_config(cert, key, retention=86400):
            config.write_text(f'listen="127.0.0.1:{port}"\ndata_dir="{data}"\ne2ee_required=false\ncertificate="{cert}"\nprivate_key="{key}"\nretention_seconds={retention}\n')

        write_config(first_cert, first_key)
        subprocess.run([str(binary), "--config", str(config), "init"], check=True, stdout=subprocess.DEVNULL)
        token = secrets.token_hex(32)
        with sqlite3.connect(data / "server.db") as connection:
            connection.execute("INSERT INTO users(id,name) VALUES(1,'TLS fixture')")
            connection.execute("INSERT INTO memberships VALUES(1,1)")
            connection.execute("INSERT INTO devices(id,user_id,name,token_hash,created_at) VALUES(1,1,'TLS fixture',?,?)",
                               (hashlib.sha256(token.encode()).hexdigest(), int(time.time())))
        logfile = root / "server.log"
        stderr = open(logfile, "wb")
        process = subprocess.Popen([str(binary), "--config", str(config), "serve"], stdout=subprocess.DEVNULL, stderr=stderr)
        ws = None
        try:
            base = f"https://localhost:{port}"
            wait_ready(base, process, context)
            fingerprint = peer_fingerprint(port, context)
            checks = {"trusted_https": True}
            for name, ctx, hostname in [("untrusted_ca_rejected", ssl.create_default_context(), "localhost"),
                                         ("wrong_hostname_rejected", context, "wrong.invalid")]:
                try:
                    peer_fingerprint(port, ctx, hostname)
                except ssl.SSLCertVerificationError:
                    checks[name] = True
                else:
                    raise AssertionError(name)
            ws = WebSocket("localhost", port, token, tls=context)
            receive_until(ws, "synced")
            checks["trusted_wss"] = True
            identity = str(uuid.uuid4())
            old = json_request(base, "/v2/conversations/1/messages", token,
                               {"client_message_id": identity, "text": "TLS fixture"}, context)
            received = receive_until(ws, "message")
            assert received["message"]["id"] == old["id"]
            assert old["expires_at"] - old["created_at"] == 86400
            ws.ack(old["seq"])
            before = len(logfile.read_text().splitlines())
            write_config(second_cert, second_key, 120)
            os.kill(process.pid, signal.SIGHUP)
            assert "обновлены" in wait_logged(logfile, before)
            fingerprint_after = peer_fingerprint(port, context)
            assert fingerprint_after != fingerprint
            checks["valid_rotation"] = True
            new = json_request(base, "/v2/conversations/1/messages", token,
                               {"client_message_id": str(uuid.uuid4()), "text": "After rotation"}, context)
            assert new["expires_at"] - new["created_at"] == 120
            received = receive_until(ws, "message")
            assert received["message"]["id"] == new["id"]
            ws.ack(new["seq"])
            retry = json_request(base, "/v2/conversations/1/messages", token,
                                 {"client_message_id": identity, "text": "TLS fixture"}, context)
            assert retry == old
            checks["existing_stream_survives_rotation"] = checks["retention_only_changes_new_messages"] = True
            malformed = root / "malformed.pem"
            malformed.write_text("invalid certificate")
            for name, cert, key in [("malformed_reload_preserves_certificate", malformed, second_key),
                                    ("expired_reload_preserves_certificate", expired_cert, expired_key),
                                    ("not_yet_valid_reload_preserves_certificate", future_cert, future_key),
                                    ("mismatched_key_preserves_certificate", second_cert, first_key)]:
                before = len(logfile.read_text().splitlines())
                write_config(cert, key)
                os.kill(process.pid, signal.SIGHUP)
                assert "отклонено" in wait_logged(logfile, before)
                assert peer_fingerprint(port, context) == fingerprint_after
                json_request(base, "/healthz", context=context)
                checks[name] = True
            write_config(second_cert, second_key, 120)
            subprocess.run([str(binary), "--config", str(config), "revoke", "1"], check=True, stdout=subprocess.DEVNULL)
            deadline = time.monotonic() + 3
            closed = False
            while time.monotonic() < deadline:
                try:
                    payload = ws.socket.recv(65536)
                    if not payload:
                        closed = True
                        break
                    ws.events(payload)
                except (EOFError, ConnectionResetError):
                    closed = True
                    break
            assert closed
            checks["private_admin_socket_revokes_live_stream"] = True
            assert token not in logfile.read_text()
            assert "TLS fixture" not in logfile.read_text()
            checks["no_tokens_or_text_in_server_log"] = True
            result = {"passed": all(checks.values()), "checks": checks}
            encoded = json.dumps(result, indent=2)
            print(encoded)
            if report_path:
                Path(report_path).write_text(encoded + "\n")
            return 0
        finally:
            if ws:
                ws.close()
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
    parser.add_argument("--report")
    arguments = parser.parse_args()
    raise SystemExit(run(Path(arguments.binary).resolve(), arguments.report))
