#!/usr/bin/env python3
"""Connected Linux GUI acceptance and RSS measurement on an isolated X11 display.

Prepare a disposable enrolled Linux profile. Run with DISPLAY set to Xvfb:
  python3 crates/desktop/tests/gui_smoke.py --binary /path/poknite \
    --profile-dir /tmp/disposable-profile --ca /tmp/test-ca.pem
The script publishes two test texts to that profile's server/channel, hides the
window, verifies receipt while hidden, and reopens the same running instance.
With --switch-channel, also verifies drafts, search, a full process restart and
Ctrl+Enter sending to the restored channel. Uses stdlib, libX11 and libXtst.
No tokens or message text appear in the report. Screenshots use test data only.
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
import tempfile
import urllib.request
import uuid
parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--profile-dir',type=Path,required=True)
parser.add_argument('--publisher-profile-dir',type=Path)
parser.add_argument('--ca',type=Path)
parser.add_argument('--dev-http',action='store_true')
parser.add_argument('--channel',type=int,default=1)
parser.add_argument('--switch-channel',type=int)
parser.add_argument('--screenshot',type=Path)
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
class XError(C.Structure):
    _fields_=[('type',C.c_int),('display',C.c_void_p),('resourceid',C.c_ulong),('serial',C.c_ulong),('error_code',C.c_ubyte),('request_code',C.c_ubyte),('minor_code',C.c_ubyte)]
x_errors=[]
@C.CFUNCTYPE(C.c_int,C.c_void_p,C.POINTER(XError))
def x_error_handler(_,error):
    # Root children can disappear between XQueryTree and XFetchName.
    if error.contents.error_code!=3:x_errors.append(error.contents.error_code)
    return 0
x.XSetErrorHandler.argtypes=[C.c_void_p]
x.XSetErrorHandler(C.cast(x_error_handler,C.c_void_p))
class Data(C.Union):_fields_=[('b',C.c_char*20),('s',C.c_short*10),('l',C.c_long*5)]
class Client(C.Structure):_fields_=[('type',C.c_int),('serial',C.c_ulong),('send_event',C.c_int),('display',C.c_void_p),('window',C.c_ulong),('message_type',C.c_ulong),('format',C.c_int),('data',Data)]
class Event(C.Union):_fields_=[('client',Client),('padding',C.c_long*24)]
def windows(title=b'Poknite'):
    children=C.POINTER(C.c_ulong)();count=C.c_uint();a=C.c_ulong();b=C.c_ulong()
    x.XQueryTree(display,root,C.byref(a),C.byref(b),C.byref(children),C.byref(count));found=set()
    for i in range(count.value):
        name=C.c_char_p();x.XFetchName(display,children[i],C.byref(name))
        if name.value==title:found.add(children[i])
        if name:x.XFree(name)
    if children:x.XFree(children)
    if x_errors:raise RuntimeError(f'X11 errors: {x_errors}')
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
    with tempfile.TemporaryDirectory(prefix='poknite-gui-publish-') as folder:
        path=Path(folder)/'text.txt';path.write_text('Проверка фонового получения Poknite',encoding='utf-8')
        command=[str(args.binary),'--data-dir',str(args.publisher_profile_dir or directory),'--headless','send','--channel',str(args.channel),'--text-file',str(path)]
        if args.ca:command+=['--ca',str(args.ca)]
        result=subprocess.run(command,check=True,capture_output=True,text=True,timeout=15)
        return json.loads(result.stdout)

def rss():
    for line in Path(f'/proc/{process.pid}/status').read_text().splitlines():
        if line.startswith('VmRSS:'):return int(line.split()[1])*1024
    raise RuntimeError('RSS unavailable')

def ticks():
    stat=Path(f'/proc/{process.pid}/stat').read_text().rsplit(')',1)[1].split()
    return int(stat[11])+int(stat[12])

def selection():
    with sqlite3.connect(directory/'history.db') as db:
        row=db.execute("SELECT value FROM meta WHERE key='selected_channel'").fetchone()
        return row[0] if row else 0

def drafts():
    with sqlite3.connect(directory/'history.db') as db:
        return dict(db.execute('SELECT channel_id,text FROM drafts'))

def stop():
    process.terminate()
    try:process.wait(timeout=5)
    except subprocess.TimeoutExpired:process.kill();process.wait()
    process.stderr.close()

def controls(window):
    x.XSetInputFocus.argtypes=[C.c_void_p,C.c_ulong,C.c_int,C.c_ulong]
    x.XSetInputFocus(display,window,2,0);x.XFlush(display)
    xt=C.CDLL('libXtst.so.6')
    xt.XTestFakeMotionEvent.argtypes=[C.c_void_p,C.c_int,C.c_int,C.c_int,C.c_ulong]
    xt.XTestFakeButtonEvent.argtypes=[C.c_void_p,C.c_uint,C.c_int,C.c_ulong]
    xt.XTestFakeKeyEvent.argtypes=[C.c_void_p,C.c_uint,C.c_int,C.c_ulong]
    x.XGetGeometry.argtypes=[C.c_void_p,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_int),C.POINTER(C.c_int),C.POINTER(C.c_uint),C.POINTER(C.c_uint),C.POINTER(C.c_uint),C.POINTER(C.c_uint)]
    a=C.c_ulong();left=C.c_int();top=C.c_int();width=C.c_uint();height=C.c_uint();border=C.c_uint();depth=C.c_uint()
    x.XGetGeometry(display,window,C.byref(a),C.byref(left),C.byref(top),C.byref(width),C.byref(height),C.byref(border),C.byref(depth))
    x.XStringToKeysym.argtypes=[C.c_char_p];x.XStringToKeysym.restype=C.c_ulong
    x.XKeysymToKeycode.argtypes=[C.c_void_p,C.c_ulong];x.XKeysymToKeycode.restype=C.c_uint
    def click(px,py):
        xt.XTestFakeMotionEvent(display,0,left.value+px,top.value+py,0)
        xt.XTestFakeButtonEvent(display,1,1,0);xt.XTestFakeButtonEvent(display,1,0,0);x.XFlush(display)
    def key(name,down=True):
        code=x.XKeysymToKeycode(display,x.XStringToKeysym(name.encode()))
        assert code,'No keycode for test input'
        xt.XTestFakeKeyEvent(display,code,int(down),0);x.XFlush(display)
    def press(name):key(name);key(name,False)
    return click,key,press

def screenshot():
    if args.screenshot:
        args.screenshot.parent.mkdir(parents=True,exist_ok=True)
        subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-f','x11grab','-video_size','1280x900','-i',os.environ['DISPLAY'],'-frames:v','1','-y',str(args.screenshot)],check=True,timeout=15)

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
    click,key,press=controls(next(iter(windows()-before)))
    click(720,170)
    dialog_title='Шифрование разговора'.encode()
    wait(lambda:bool(windows(dialog_title)),'Encryption dialog did not open')
    click,key,press=controls(next(iter(windows(dialog_title))))
    click(340,370)  # Show the disposable conversation code.
    time.sleep(.2)
    click(560,370)  # Import the same code using the dialog.
    time.sleep(.2)
    click(560,420)
    wait(lambda:not(windows(dialog_title)),'Encryption dialog did not close')
    result['encryption_dialog_export_import']=True
    if args.switch_channel:
        channel=args.switch_channel
        initial=drafts()
        assert initial[args.channel] and initial[channel],'Seed both channel drafts in the isolated fixture'
        click,key,press=controls(next(iter(windows()-before)))
        click(80,217)
        wait(lambda:selection()==channel,'Second channel was not selected')
        click(80,199)
        wait(lambda:selection()==args.channel,'First channel was not selected')
        assert drafts()[channel]==initial[channel],'Switching replaced the second draft'
        click(80,217)
        wait(lambda:selection()==channel,'Second channel selection was not saved')
        click(80,160)
        for letter in 'work':press(letter)
        time.sleep(.3)
        click(80,199)
        time.sleep(.3)
        assert selection()==channel,'Filtered sidebar selected the wrong conversation'
        click(80,160);key('Control_L');press('a');key('Control_L',False);press('BackSpace')
        time.sleep(.3)
        screenshot()
        stop()
        process=subprocess.Popen(command,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        wait(lambda:bool(windows()-before),'Fresh launch did not show a window')
        time.sleep(2)
        assert selection()==channel,'Fresh launch forgot the selected conversation'
        click,key,press=controls(next(iter(windows()-before)))
        click(400,560);key('Control_L');press('Return');key('Control_L',False)
        def sent_to_restored_channel():
            with sqlite3.connect(directory/'history.db') as db:
                return bool(db.execute('SELECT 1 FROM messages WHERE channel_id=? AND sender_id=? AND text=?',(channel,profile['user_id'],initial[channel])).fetchone())
        try:wait(sent_to_restored_channel,'Restored composer did not send its draft with Ctrl+Enter')
        except AssertionError:
            screenshot()
            raise
        assert drafts()[args.channel]==initial[args.channel],'Sending changed the other channel draft'
        time.sleep(.3)
        screenshot()
        result.update({'channel_switch_drafts_preserved':True,'sidebar_search':True,'full_restart_restores_channel':True,'ctrl_enter_sends_restored_draft':True})
    else:screenshot()
    print(json.dumps(result,indent=2))
    if args.report:args.report.write_text(json.dumps(result,indent=2)+'\n')
finally:
    stop();x.XCloseDisplay(display)
