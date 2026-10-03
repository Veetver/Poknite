#!/usr/bin/env python3
"""Exercise the released desktop core against real, certificate-validated HTTPS/WSS."""
import argparse
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import uuid

def execute(args, **kwargs):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, **kwargs)
    if result.returncode:
        # Deliberately omit stdout: successful invitation commands print secrets there.
        raise RuntimeError(f'{Path(str(args[0])).name} failed ({result.returncode}): {result.stderr[-6000:]}')
    return result.stdout.strip()

def wait(predicate, seconds=12):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError('Timed out waiting for delivery')

def contains(directory, identifier):
    try:
        with sqlite3.connect(directory / 'history.db') as database:
            return database.execute('SELECT COUNT(*) FROM messages WHERE id=?', (identifier,)).fetchone()[0] == 1
    except sqlite3.OperationalError:
        return False

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--server', type=Path, default=Path('target/release/poknited'))
    parser.add_argument('--client', type=Path, default=Path('target/release/poknite'))
    parser.add_argument('--output', type=Path)
    parser.add_argument('--gui-report', type=Path, help='Additionally test connected GUI on an isolated DISPLAY')
    args = parser.parse_args()
    server_binary, client_binary = args.server.resolve(), args.client.resolve()
    children = []
    handles = []
    with tempfile.TemporaryDirectory(prefix='poknite-client-smoke-') as temporary:
        root = Path(temporary)
        key, cert, ca = root / 'key.pem', root / 'cert.pem', root / 'ca.pem'
        execute(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                 '-subj', '/CN=Poknite isolated test CA', '-addext', 'basicConstraints=critical,CA:TRUE',
                 '-addext', 'keyUsage=critical,keyCertSign,cRLSign',
                 '-keyout', root / 'ca.key', '-out', ca])
        execute(['openssl', 'req', '-new', '-newkey', 'rsa:2048', '-nodes',
                 '-subj', '/CN=127.0.0.1', '-keyout', key, '-out', root / 'server.csr'])
        extensions = root / 'extensions.cnf'
        extensions.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n'
                              'extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n')
        execute(['openssl', 'x509', '-req', '-in', root / 'server.csr', '-CA', ca,
                 '-CAkey', root / 'ca.key', '-CAcreateserial', '-days', '1',
                 '-extfile', extensions, '-out', cert])
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        config = root / 'poknite.toml'
        config.write_text(f'listen="127.0.0.1:{port}"\ndata_dir="{root / "server"}"\n'
                          f'certificate="{cert}"\nprivate_key="{key}"\nretention_seconds=2\n')
        admin = [server_binary, '--config', config]
        execute([*admin, 'init'])
        execute([*admin, 'user', 'Receiver'])
        execute([*admin, 'user', 'Sender'])
        if args.gui_report:
            execute([*admin, 'channel', 'Рабочий · work'])
            execute([*admin, 'grant', '1', '2'])
        log = (root / 'server.log').open('w')
        handles.append(log)
        server = subprocess.Popen([str(a) for a in [*admin, 'serve']], stdout=log, stderr=log)
        children.append(server)
        def ready():
            if server.poll() is not None:
                raise AssertionError('Server did not start')
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.2):
                    return True
            except OSError:
                return False
        try:
            wait(ready)
            profiles = [root / 'computer', root / 'phone-core', root / 'sender']
            def command(index, action, *extra):
                return execute([client_binary, '--data-dir', profiles[index], '--ca', ca,
                                '--headless', action, *extra])
            for i, user in enumerate([1, 1, 2]):
                invite = root / f'invitation-{i}'
                invite.write_text(execute([*admin, 'invite', user]))
                os.chmod(invite, 0o600)
                command(i, 'enroll', '--server', f'https://127.0.0.1:{port}',
                        '--invitation-file', invite, '--name', f'Device-{i}')
                assert (profiles[i] / 'profile.json').stat().st_mode & 0o077 == 0
            # The code is moved locally between disposable trusted profiles, never HTTP.
            group_code = root / 'group.code'
            command(0, 'key-create', '--channel', '1', '--key-file', group_code)
            for i in [1, 2]:
                command(i, 'key-import', '--channel', '1', '--key-file', group_code)
            def run(index):
                path = root / f'client-{index}-{len(children)}.log'
                handle = path.open('w')
                handles.append(handle)
                process = subprocess.Popen([str(client_binary), '--data-dir', str(profiles[index]),
                                            '--ca', str(ca), '--headless', 'run', '--seconds', '60', '--no-notify'],
                                           stdout=handle, stderr=subprocess.STDOUT)
                children.append(process)
                wait(lambda: '"status":"connected"' in path.read_text())
                return process, path
            first, first_log = run(0)
            second, second_log = run(1)
            text = root / 'text.txt'
            text.write_text('Проверка общей доставки', encoding='utf-8')
            one = json.loads(command(2, 'send', '--channel', '1', '--text-file', text))
            wait(lambda: all(contains(p, one['id']) for p in profiles[:2]))
            second.terminate()
            second.wait(timeout=5)
            text.write_text('Сообщение пока второе устройство отключено', encoding='utf-8')
            two = json.loads(command(2, 'send', '--channel', '1', '--text-file', text))
            wait(lambda: contains(profiles[0], two['id']))
            second, second_log = run(1)
            wait(lambda: contains(profiles[1], two['id']))
            time.sleep(2.1)
            execute([*admin, 'cleanup'])
            with sqlite3.connect(root / 'server/server.db') as database:
                assert database.execute('SELECT COUNT(*) FROM messages').fetchone()[0] == 0
            assert all(contains(p, one['id']) and contains(p, two['id']) for p in profiles[:2])
            device = json.loads(command(1, 'status'))['device_id']
            command(0, 'revoke', '--device', str(device))
            wait(lambda: '"status":"revoked"' in second_log.read_text())
            # The production client rejects untrusted test certificates; no insecure TLS switch exists.
            bad = subprocess.run([str(client_binary), '--data-dir', str(profiles[2]),
                                  '--headless', 'devices'], capture_output=True)
            assert bad.returncode != 0
            if args.gui_report:
                # The test revoked a device: rotate before the GUI sends again.
                rotated_code = root / 'group-rotated.code'
                command(0, 'key-create', '--channel', '1', '--key-file', rotated_code)
                command(2, 'key-import', '--channel', '1', '--key-file', rotated_code)
                command(0, 'key-create', '--channel', '2', '--key-file', root / 'work.code')
                first.terminate()
                first.wait(timeout=5)
                with sqlite3.connect(profiles[0] / 'history.db') as database:
                    database.execute("INSERT INTO meta VALUES('selected_channel',1) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                    for channel, value in [(1, 'Черновик для общего канала'), (2, 'Привет! Продолжаем обсуждение здесь.')]:
                        database.execute('INSERT OR REPLACE INTO drafts(channel_id,text,message_id) VALUES(?,?,?)', (channel, value, str(uuid.uuid4())))
                args.gui_report.parent.mkdir(parents=True, exist_ok=True)
                execute(['python3', Path(__file__).resolve().parents[1] / 'crates/desktop/tests/gui_smoke.py',
                         '--binary', client_binary, '--profile-dir', profiles[0], '--publisher-profile-dir', profiles[2], '--ca', ca,
                         '--switch-channel', '2', '--screenshot', args.gui_report.with_suffix('.png').resolve(),
                         '--report', args.gui_report.resolve()])
            report = {'result': 'pass', 'checks': ['validated_https_wss', 'private_profile',
                      'e2ee_device_keys', 'independent_devices', 'offline_replay', 'server_expiration',
                      'local_copy_retained', 'live_revocation', 'untrusted_tls_rejected'],
                      'devices': 3, 'messages': 2}
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
            print(json.dumps(report, ensure_ascii=False))
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            for handle in handles:
                handle.close()

if __name__ == '__main__':
    main()
