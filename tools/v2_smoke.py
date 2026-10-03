#!/usr/bin/env python3
"""Disposable public/admin TLS fixture; tests the native client IP override and SNI.
No TLS verification bypasses, production credentials, or user databases.
"""
import argparse
import hashlib
import http.client
import json
import os
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
from server_soak import json_request, wait_ready
from tls_smoke import openssl


def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--server',type=Path,default=Path('target/release/poknited'))
    parser.add_argument('--client',type=Path,default=Path('target/release/poknite'))
    parser.add_argument('--report',type=Path)
    parser.add_argument('--gui',action='store_true')
    args=parser.parse_args()
    binary=args.server.resolve(); client=args.client.resolve()
    checks={}
    with tempfile.TemporaryDirectory(prefix='poknite-v2-') as directory:
        root=Path(directory);data=root/'data';profile=root/'profile';profile.mkdir()
        public,management=port(),port()
        openssl('req','-x509','-newkey','rsa:2048','-nodes','-keyout',root/'ca.key','-out',root/'ca.pem','-days','2','-subj','/CN=Poknite isolated test CA','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,keyCertSign,cRLSign')
        openssl('req','-new','-newkey','rsa:2048','-nodes','-keyout',root/'leaf.key','-out',root/'leaf.csr','-subj','/CN=localhost')
        (root/'extensions').write_text('subjectAltName=DNS:localhost,DNS:management.poknite.test\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
        openssl('x509','-req','-in',root/'leaf.csr','-CA',root/'ca.pem','-CAkey',root/'ca.key','-set_serial','2','-days','2','-extfile',root/'extensions','-out',root/'leaf.pem')
        context=ssl.create_default_context(cafile=str(root/'ca.pem'))
        config=root/'config.toml'
        def configure(peer='127.0.0.1',subnet='127.0.0.0/8'):
            config.write_text(f'listen="127.0.0.1:{public}"\ndata_dir="{data}"\ncertificate="{root / "leaf.pem"}"\nprivate_key="{root / "leaf.key"}"\n[management]\nenabled=true\nlisten="{peer}:{management}"\nallowed_subnets=["{subnet}"]\n')
        configure()
        subprocess.run([str(binary),'--config',str(config),'init'],check=True,stdout=subprocess.DEVNULL)
        token=secrets.token_hex(32)
        with sqlite3.connect(data/'server.db') as c:
            c.execute("INSERT INTO users(id,name) VALUES(1,'Администратор'),(2,'Собеседник')")
            c.execute('INSERT INTO user_roles VALUES(1,1)')
            c.execute('INSERT INTO memberships VALUES(1,1),(2,1)')
            c.execute("UPDATE users SET color='#cc2266' WHERE id IN (1,2)")
            c.execute("INSERT INTO devices(id,user_id,name,token_hash,created_at) VALUES(1,1,'TLS fixture',?,?)",(hashlib.sha256(token.encode()).hexdigest(),int(time.time())))
        saved={'server':f'https://localhost:{public}','token':token,'user_id':1,'device_id':1,'user_name':'Администратор','management_server':f'https://management.poknite.test:{management}','management_ip':'127.0.0.1'}
        def save():
            (profile/'profile.json').write_text(json.dumps(saved))
            os.chmod(profile/'profile.json',0o600)
        save()
        def command(action,*extra):
            return subprocess.run([str(client),'--data-dir',str(profile),'--ca',str(root/'ca.pem'),'--headless',action,*extra],capture_output=True,text=True,timeout=30)
        handle=(root/'server.log').open('wb')
        process=None;gui=None
        try:
            def start():
                return subprocess.Popen([str(binary),'--config',str(config),'serve'],stdout=subprocess.DEVNULL,stderr=handle)
            process=start();wait_ready(saved['server'],process,context)
            result=command('management-check');assert result.returncode==0,result.stderr;assert json.loads(result.stdout)['available'];checks['native_admin_ip_override_with_domain_tls']=True
            for path,status in [('/v2/admin/roles',404),('/v1/stream',426)]:
                try:json_request(saved['server'],path,token,context=context)
                except urllib.error.HTTPError as e:assert e.code==status
                else:raise AssertionError(path)
            checks['public_admin_absent_and_v1_upgrade_required']=True
            saved['management_server']=f'https://wrong.invalid:{management}';save();result=command('management-check');assert result.returncode!=0;assert 'сертиф' in result.stderr.lower() or 'cert' in result.stderr.lower();checks['override_still_rejects_wrong_certificate_name']=True
            saved['management_server']=f'https://management.poknite.test:{management}';saved['management_ip']='127.0.0.2';save();result=command('management-check');assert result.returncode!=0;assert 'Управление недоступно из текущей сети' in result.stderr
            json_request(saved['server'],'/healthz',context=context);checks['admin_unreachable_public_remains_available']=True
            saved['management_ip']='127.0.0.1';save()
            if args.gui:
                result=command('key-create','--channel','1','--key-file',str(root/'group.code'));assert result.returncode==0,result.stderr
                (root/'text.txt').write_text('Проверка цвета ника и истории')
                result=command('send','--channel','1','--text-file',str(root/'text.txt'));assert result.returncode==0,result.stderr
                subprocess.run([str(client),'--data-dir',str(profile),'--ca',str(root/'ca.pem'),'--headless','run','--seconds','2','--no-notify'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                gui=subprocess.Popen([str(client),'--data-dir',str(profile),'--ca',str(root/'ca.pem')],stdout=subprocess.DEVNULL,stderr=(root/'gui.log').open('w'))
                time.sleep(3)
                if args.report:
                    # Optional captures from the isolated X11 display for visual review.
                    capture=args.report.with_suffix('.png')
                    subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-f','x11grab','-video_size','1280x900','-i',os.environ['DISPLAY'],'-frames:v','1',str(capture)],check=True)
                time.sleep(40)
                assert gui.poll() is None;gui.terminate();gui.wait(timeout=5);gui=None
                checks['native_gui_stays_running']=True
            process.terminate();process.wait(timeout=5);process=None
            configure('127.0.0.2','127.0.0.2/32');process=start();wait_ready(saved['server'],process,context)
            # Socket target differs from TLS name; source is actual 127.0.0.1.
            with socket.create_connection(('127.0.0.2',management),timeout=5) as raw:
                with context.wrap_socket(raw,server_hostname='localhost') as tls:
                    tls.sendall((f'GET /v2/admin/roles HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nX-Forwarded-For: 127.0.0.2\r\nForwarded: for=127.0.0.2\r\nConnection: close\r\n\r\n').encode())
                    response=http.client.HTTPResponse(tls);response.begin();assert response.status==403
            checks['real_tls_peer_cidr_rejects_spoofed_headers']=True
        finally:
            if gui is not None:gui.terminate();gui.wait(timeout=5)
            if process is not None:process.terminate();process.wait(timeout=5)
            handle.close()
    report={'checks':checks}
    if args.report:args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))


if __name__=='__main__':main()
