#!/usr/bin/env python3
"""Real ENOSPC acceptance on an isolated Docker tmpfs; never fills host storage.

Requires Docker, OpenSSL and Python standard library. The supplied Ubuntu 24.04
server executable and temporary TLS fixtures are mounted read-only. Only one
random loopback HTTPS port is published; /data is a disposable 512 KiB/1 MiB tmpfs.
"""
import argparse
import json
from pathlib import Path
import secrets
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import time
import urllib.error
import uuid
from server_soak import WebSocket, json_request
from tls_smoke import certificate, openssl


def run(binary, image, report_path, tmpfs_bytes):
    binary = Path(binary).resolve(strict=True)
    assert tmpfs_bytes in (524288, 1048576), "Only scoped 512 KiB/1 MiB test mounts are permitted"
    fixture_users = 1 if tmpfs_bytes == 524288 else 2
    fixture_devices = fixture_users * 3
    name = "poknite-enospc-" + secrets.token_hex(6)
    created = False

    def docker(*args, check=True):
        result = subprocess.run(["docker", *map(str, args)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if check and result.returncode:
            # No sensitive fixture values are passed as Docker arguments or printed.
            raise RuntimeError("Scoped Docker operation failed: " + result.stderr.strip())
        return result.stdout.strip()

    with tempfile.TemporaryDirectory(prefix="poknite-enospc-") as temporary:
        root = Path(temporary)
        openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "2",
                "-keyout", root / "ca.key", "-out", root / "ca.pem", "-subj", "/CN=Poknite disposable ENOSPC CA",
                "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign")
        certificate(root, "server")
        root.joinpath("poknite.toml").write_text('listen="0.0.0.0:8443"\ndata_dir="/data"\ne2ee_required=false\ncertificate="/fixture/server.pem"\nprivate_key="/fixture/server.key"\n')
        context = ssl.create_default_context(cafile=str(root / "ca.pem"))
        reserved = socket.socket()
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
        reserved.close()
        base = f"https://localhost:{port}"
        # All startup credentials are written to the private tmpfs, never logs.
        setup = ["umask 077", "/opt/poknited --config /fixture/poknite.toml init >/dev/null"]
        for user in range(1, fixture_users + 1):
            setup.append(f"/opt/poknited --config /fixture/poknite.toml user DiskFixture{user} >/dev/null")
            for device in range((user - 1) * 3 + 1, user * 3 + 1):
                setup.append(f"/opt/poknited --config /fixture/poknite.toml invite {user} > /data/invite{device}")
        setup.append("exec /opt/poknited --config /fixture/poknite.toml serve")
        startup = " && ".join(setup)
        try:
            # Docker can create a named container before failing to start it.
            created = True
            docker("run", "--detach", "--name", name, "--read-only", "--user", "1000:1000", "--cap-drop", "ALL",
                   "--security-opt", "no-new-privileges:true", "--pids-limit", "64", "--memory", "128m",
                   "--tmpfs", f"/data:rw,size={tmpfs_bytes},mode=0700,uid=1000,gid=1000",
                   "--mount", f"type=bind,src={binary},dst=/opt/poknited,readonly",
                   "--mount", f"type=bind,src={root},dst=/fixture,readonly",
                   "--publish", f"127.0.0.1:{port}:8443", image, "sh", "-ec", startup)
            for _ in range(100):
                try:
                    if json_request(base, "/healthz", context=context)["status"] == "ok":
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise RuntimeError("Scoped HTTPS server did not become ready")
            tokens = []
            for n in range(1, fixture_devices + 1):
                invite = docker("exec", name, "cat", f"/data/invite{n}")
                enrolled = json_request(base, "/v2/devices/enroll", body={"invitation": invite, "device_name": f"Disk fixture {n}"}, context=context)
                tokens.append(enrolled["token"])
                docker("exec", name, "rm", f"/data/invite{n}")
            marker = "disk-full-fixture-" + secrets.token_hex(12)
            confirmed = []
            rejected_status = rejected_code = None
            for attempt in range(240):
                token = tokens[attempt % len(tokens)]
                text = marker + "-" + str(attempt)
                text += "x" * (4096 - len(text))
                payload = {"client_message_id": str(uuid.uuid4()), "text": text}
                try:
                    message = json_request(base, "/v2/conversations/1/messages", token, payload, context)
                    confirmed.append((token, payload, message))
                except urllib.error.HTTPError as error:
                    rejected_status = error.code
                    rejected_code = json.loads(error.read())["code"]
                    break
            assert confirmed, "Disk exhausted before any accepted message"
            assert rejected_status == 507 and rejected_code == "storage_full", f"Expected 507 storage_full, got {rejected_status}/{rejected_code}"

            # A direct bounded write proves a real kernel ENOSPC, independently of
            # SQLite's max_page_count. The probe never leaves this tiny tmpfs.
            block, total, free = map(int, docker("exec", name, "stat", "-f", "--format=%S %b %a", "/data").split())
            probe = subprocess.run(["docker", "exec", name, "env", "LC_ALL=C", "dd", "if=/dev/zero", "of=/data/ENOSPC-probe", "bs=4096", "count=300", "status=none"], capture_output=True, text=True)
            kernel_enospc = probe.returncode != 0 and "No space left on device" in probe.stderr
            docker("exec", name, "rm", "-f", "/data/ENOSPC-probe")
            # The image deliberately needs no Python/SQLite CLI. Copy the bounded
            # quiescent DB+WAL to the private temporary directory for host analysis.
            snapshot = root / "snapshot"
            snapshot.mkdir()
            # `docker cp` may inspect the image mount namespace rather than its
            # live tmpfs. Read scoped files through the live unprivileged process.
            for filename in ("server.db", "server.db-wal"):
                with snapshot.joinpath(filename).open("wb") as output:
                    result = subprocess.run(["docker", "exec", name, "cat", "/data/" + filename], stdout=output, stderr=subprocess.PIPE)
                assert result.returncode == 0, "Cannot read scoped database snapshot"
            with sqlite3.connect(snapshot / "server.db") as copied:
                storage = {"filesystem_capacity_bytes": block * total, "free_bytes_at_rejection": block * free,
                           "kernel_enospc": kernel_enospc, "database_page_count": copied.execute("pragma page_count").fetchone()[0],
                           "message_count": copied.execute("select count(*) from messages").fetchone()[0],
                           "integrity": copied.execute("pragma integrity_check").fetchone()[0]}
            assert storage["filesystem_capacity_bytes"] == tmpfs_bytes and storage["kernel_enospc"]
            assert storage["database_page_count"] * 4096 < 32 * 1024 * 1024
            assert storage["message_count"] == len(confirmed) and storage["integrity"] == "ok"

            token, payload, original = confirmed[0]
            retry_status = 200
            try:
                retried = json_request(base, "/v2/conversations/1/messages", token, payload, context)
                assert retried["id"] == original["id"] and retried["text"] == original["text"]
            except urllib.error.HTTPError as error:
                retry_status = error.code
                assert retry_status in (503, 507), "Unexpected retry failure"
            assert json_request(base, "/healthz", context=context)["status"] == "ok"
            assert json_request(base, "/v2/channels", tokens[0], context=context)[0]["id"] == 1
            received = {}
            ws = WebSocket("localhost", port, tokens[0], tls=context)
            try:
                synced = False
                while not synced:
                    for event in ws.events():
                        if event["type"] == "message":
                            received[event["message"]["id"]] = event["message"]["text"]
                        elif event["type"] == "synced":
                            synced = True
                    if not synced:
                        part = ws.socket.recv(65536)
                        assert part, "Stream closed after ENOSPC"
                        ws.buffer.extend(part)
            finally:
                ws.close()
            assert received == {m["id"]: m["text"] for _, _, m in confirmed}, "Accepted live messages lost after disk exhaustion"
            logs = docker("logs", name)
            # Docker writes container logs to stderr in some client versions.
            log_result = subprocess.run(["docker", "logs", name], capture_output=True, text=True)
            logs += log_result.stderr
            assert not any(value in logs for value in tokens) and marker not in logs
            assert docker("inspect", "--format", "{{.State.Running}}", name) == "true"
            report = {"passed": True, "scenario": "real ENOSPC on private Docker tmpfs, trusted HTTPS/WSS", "os": "Ubuntu 24.04 container", "tmpfs_bytes": tmpfs_bytes,
                      "configured_database_budget_bytes": 32 * 1024 * 1024, "fixture_devices": fixture_devices, "confirmed_messages": len(confirmed), "text_bytes": 4096,
                      "rejected_http_status": rejected_status, "rejected_code": rejected_code, "idempotent_retry_http_status": retry_status,
                      "storage": storage, "checks": {"kernel_enospc": True, "database_below_configured_quota": True, "previous_live_messages_retained": True,
                      "wss_replay_readable_after_disk_full": True, "authenticated_reads_work": True, "process_alive": True, "no_tokens_or_text_in_logs": True},
                      "host_disk_filled": False, "loopback_only_published_port": True}
            report_path = Path(report_path)
            report_path.parent.mkdir(parents=True, exist_ok=True)
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2))
        finally:
            if created:
                docker("rm", "--force", name, check=False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/poknited")
    parser.add_argument("--image", default="poknite-build-ubuntu24-dbus")
    parser.add_argument("--tmpfs-bytes", type=int, choices=(524288, 1048576), default=524288)
    parser.add_argument("--report", default="dist/reports/server-real-disk-full.json")
    args = parser.parse_args()
    run(args.binary, args.image, args.report, args.tmpfs_bytes)
