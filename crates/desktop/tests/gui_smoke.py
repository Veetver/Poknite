#!/usr/bin/env python3
"""Connected Linux GUI acceptance and RSS measurement on an isolated X11 display.

Prepare a disposable enrolled Linux profile. Run with DISPLAY set to Xvfb:
  python3 crates/desktop/tests/gui_smoke.py --binary /path/poknite \
    --profile-dir /tmp/disposable-profile --ca /tmp/test-ca.pem
The script publishes two test texts to that profile's server/channel, hides the
window, verifies receipt while hidden, and reopens the same running instance.
No tokens or message text appear in the report. Uses Python stdlib and libX11.
"""
import argparse
import ctypes as C
import json
import os
from pathlib import Path
import sqlite3
import ssl
import subprocess
import time
import urllib.request
import uuid
parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--profile-dir',type=Path,required=True)
parser.add_argument('--publisher-profile-dir',type=Path)
parser.add_argument('--ca',type=Path)
parser.add_argument('--dev-http',action='store_true')
parser.add_argument('--channel',type=int,default=1)
parser.add_argument('--report',type=Path)
args=parser.parse_args()
binary=str(args.binary.resolve()); directory=args.profile_dir.resolve()
profile=json.loads((directory/'profile.json').read_text())
publisher=json.loads(((args.publisher_profile_dir or directory)/'profile.json').read_text())
context=ssl.create_default_context(cafile=str(args.ca) if args.ca else None)
x=C.CDLL('libX11.so.6')
x.XOpenDisplay.argtypes=[C.c_char_p];x.XOpenDisplay.restype=C.c_void_p
display=x.XOpenDisplay(None)
if not display:raise RuntimeError('Set DISPLAY to an isolated running Xvfb display')
x.XDefaultRootWindow.argtypes=[C.c_void_p];x.XDefaultRootWindow.restype=C.c_ulong
root=x.XDefaultRootWindow(display)
x.XQueryTree.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.POINTER(C.c_ulong)),C.POINTER(C.c_uint)]
x.XFetchName.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(C.c_char_p)]
x.XFree.argtypes=[C.c_void_p]
x.XInternAtom.argtypes=[C.c_void_p,C.c_char_p,C.c_int];x.XInternAtom.restype=C.c_ulong
x.XFlush.argtypes=[C.c_void_p]
x.XSendEvent.argtypes=[C.c_void_p,C.c_ulong,C.c_int,C.c_long,C.c_void_p]
x.XCloseDisplay.argtypes=[C.c_void_p]
class Data(C.Union):_fields_=[('b',C.c_char*20),('s',C.c_short*10),('l',C.c_long*5)]
class Client(C.Structure):_fields_=[('type',C.c_int),('serial',C.c_ulong),('send_event',C.c_int),('display',C.c_void_p),('window',C.c_ulong),('message_type',C.c_ulong),('format',C.c_int),('data',Data)]
class Event(C.Union):_fields_=[('client',Client),('padding',C.c_long*24)]
def windows():
    children=C.POINTER(C.c_ulong)();count=C.c_uint();a=C.c_ulong();b=C.c_ulong()
    x.XQueryTree(display,root,C.byref(a),C.byref(b),C.byref(children),C.byref(count));found=set()
    for i in range(count.value):
        name=C.c_char_p();x.XFetchName(display,children[i],C.byref(name))
        if name.value==b'Poknite':found.add(children[i])
        if name:x.XFree(name)
    if children:x.XFree(children)
    return found

def wait(condition,message,timeout=15):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        if process.poll() is not None:raise RuntimeError('GUI process exited')
        if condition():return
        time.sleep(.1)
    raise AssertionError(message)

def received(message):
    try:
        with sqlite3.connect(directory/'history.db',timeout=.2) as db:
            return bool(db.execute('SELECT 1 FROM messages WHERE id=?',(message['id'],)).fetchone())
    except sqlite3.OperationalError:return False

def publish():
    request=urllib.request.Request(publisher['server'].rstrip('/')+f'/v1/channels/{args.channel}/messages',data=json.dumps({'client_message_id':str(uuid.uuid4()),'text':'Проверка фонового получения Poknite'}).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+publisher['token']},method='POST')
    with urllib.request.urlopen(request,context=context,timeout=10) as response:return json.load(response)

def rss():
    for line in Path(f'/proc/{process.pid}/status').read_text().splitlines():
        if line.startswith('VmRSS:'):return int(line.split()[1])*1024
    raise RuntimeError('RSS unavailable')

def ticks():
    stat=Path(f'/proc/{process.pid}/stat').read_text().rsplit(')',1)[1].split()
    return int(stat[11])+int(stat[12])

command=[binary,'--data-dir',str(directory)]
if args.ca:command+=['--ca',str(args.ca.resolve())]
if args.dev_http:command+=['--dev-http']
before=windows()
process=subprocess.Popen(command,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
try:
    wait(lambda:bool(windows()-before),'GUI window did not appear')
    first=publish();wait(lambda:received(first),'Connected GUI did not receive first message')
    time.sleep(2)
    active=rss();window=next(iter(windows()-before))
    event=Event();event.client=Client(type=33,display=display,window=window,message_type=x.XInternAtom(display,b'WM_PROTOCOLS',0),format=32)
    event.client.data.l[0]=x.XInternAtom(display,b'WM_DELETE_WINDOW',0)
    x.XSendEvent(display,window,0,0,C.byref(event));x.XFlush(display)
    wait(lambda:not(windows()-before),'Window did not hide')
    second=publish();wait(lambda:received(second),'Hidden GUI did not receive second message')
    time.sleep(2);background=rss();start=ticks();time.sleep(5);idle_ticks=ticks()-start
    repeated=subprocess.run(command,capture_output=True,timeout=5)
    assert repeated.returncode==0,'Second launch did not reuse the instance'
    wait(lambda:bool(windows()-before),'Second launch did not reopen running GUI')
    assert process.poll() is None
    result={'binary_bytes':Path(binary).stat().st_size,'connected':True,'active_rss_bytes':active,'background_rss_bytes':background,'background_idle_cpu_seconds_over_5s':idle_ticks/os.sysconf('SC_CLK_TCK'),'background_message_received':True,'single_instance_reopened':True}
    print(json.dumps(result,indent=2))
    if args.report:args.report.write_text(json.dumps(result,indent=2)+'\n')
finally:
    process.terminate()
    try:process.wait(timeout=5)
    except subprocess.TimeoutExpired:process.kill();process.wait()
    process.stderr.close();x.XCloseDisplay(display)
