#!/usr/bin/env python3
"""Isolated native StatusNotifierItem registration, properties and activation.
Ubuntu test dependencies: dbus-daemon, python3-dbus, python3-gi.
Run python3 crates/desktop/tests/tray_fixture.py [cargo-path].
Does not use or affect the user's desktop tray.
"""
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
if len(sys.argv)<2 or sys.argv[1]!='--fixture':
    with tempfile.TemporaryDirectory(prefix='poknite-tray-test-') as temporary:
        config=Path(temporary)/'bus.conf'
        config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><apparmor mode="disabled"/><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        cargo=sys.argv[1] if len(sys.argv)>1 else 'cargo'
        # First exercise the silent fallback on an empty bus.
        absent=subprocess.call(['dbus-run-session','--config-file='+str(config),'--',cargo,'test','-p','poknite-desktop','absent_tray_watcher_is_optional','--','--ignored'])
        if absent:sys.exit(absent)
        sys.exit(subprocess.call(['dbus-run-session','--config-file='+str(config),'--',sys.executable,__file__,'--fixture',cargo]))
import dbus, dbus.service, dbus.mainloop.glib
from gi.repository import GLib
dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus=dbus.SessionBus()
name=dbus.service.BusName('org.kde.StatusNotifierWatcher',bus)
failures=[]
class Watcher(dbus.service.Object):
    def __init__(self):super().__init__(name,'/StatusNotifierWatcher');self.service=None;self.activated=False;self.context=False
    @dbus.service.method('org.kde.StatusNotifierWatcher',in_signature='s')
    def RegisterStatusNotifierItem(self,service):
        assert str(service).startswith('org.kde.StatusNotifierItem-')
        self.service=str(service)
        GLib.idle_add(self.inspect)
    def fail(self,error):failures.append(error)
    def inspect(self):
        item=bus.get_object(self.service,'/StatusNotifierItem',introspect=False)
        props=dbus.Interface(item,'org.freedesktop.DBus.Properties')
        interface=dbus.Interface(item,'org.kde.StatusNotifierItem')
        def got_properties(properties):
            try:
                assert properties['Id']=='Poknite' and properties['Status']=='Active'
                assert properties['Category']=='Communications' and properties['IconName']=='dialog-information'
                assert not bool(properties['ItemIsMenu']) and str(properties['Menu'])=='/'
                assert properties['ToolTip'][2]=='Poknite'
                assert list(properties['IconPixmap'])==[]
                interface.Activate(dbus.Int32(0),dbus.Int32(0),reply_handler=activated,error_handler=self.fail)
            except Exception as error:self.fail(error)
        def activated():
            self.activated=True
            interface.ContextMenu(dbus.Int32(0),dbus.Int32(0),reply_handler=context,error_handler=self.fail)
        def context():self.context=True
        props.GetAll('org.kde.StatusNotifierItem',reply_handler=got_properties,error_handler=self.fail)
        return False
watcher=Watcher();loop=GLib.MainLoop();result=[]
def run():
    result.append(subprocess.call([sys.argv[2],'test','-p','poknite-desktop','tray_registration_properties_activation_and_cleanup','--','--ignored']))
    GLib.idle_add(loop.quit)
threading.Thread(target=run).start();loop.run()
assert not failures,failures
assert watcher.service and watcher.activated and watcher.context
assert not bus.name_has_owner(watcher.service),'tray connection was not released'
sys.exit(result[0])
